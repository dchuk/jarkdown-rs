# Architecture

This document describes how jarkdown-rs is layered and how data flows through
the codebase. It complements:

- `CONTEXT.md` — domain glossary (Issue, Changelog, Comment, Worklog).
- `docs/adr/0001-changelog-export.md` — why the changelog is a sibling artifact
  with one-time backfill semantics.
- `docs/adr/0002-typed-issue-model-retains-raw-payload.md` — why `Issue` keeps
  both the typed spine and the raw `Value`.

All diagrams are MermaidJS and render natively on GitHub and most modern
Markdown viewers.

## Overall architecture

Five layers, each depending only on the ones below it. The CLI is a thin shell
over the library; everything reusable lives behind `src/lib.rs`.

```mermaid
flowchart TB
    subgraph entry[Entry surface]
        CLI["CLI bin<br/><code>jarkdown-rs</code><br/>(main.rs · cli.rs)"]
        LIB["Library API<br/>(lib.rs:<br/><code>export_issue</code> · <code>ExportOptions</code>)"]
    end

    subgraph orch[Orchestration]
        EXPORT["<code>export::perform_export_with_options</code><br/>single-issue workflow"]
        BULK["<code>bulk::BulkExporter</code><br/>semaphore-bounded fan-out"]
        HIER["<code>hierarchy::HierarchyExporter</code><br/>recursive tree traversal"]
        EXSEAM["<code>exporter::IssueExporter</code><br/>trait seam"]
    end

    subgraph domain[HTTP &amp; domain]
        CLIENT["<code>jira_client::JiraApiClient</code><br/>REST · auth · pagination"]
        ISSUE["<code>issue::Issue</code><br/>typed + raw payload"]
        CL["<code>changelog::*</code><br/>render + write"]
        RETRY["<code>retry::retry_with_backoff</code>"]
    end

    subgraph render[Rendering · pure]
        COMPOSE["<code>markdown::compose(&amp;RenderContext)</code>"]
        SECTIONS["<code>markdown::sections</code><br/>frontmatter · description · ..."]
        ADF["<code>markdown::adf</code><br/>ADF → MD"]
        HTML["<code>markdown::html</code><br/>HTML → MD"]
        ATTIDX["<code>markdown::attachments::AttachmentIndex</code>"]
        CFR["<code>custom_field::CustomFieldRenderer</code>"]
    end

    subgraph state[Persistence &amp; caches]
        MAN["<code>manifest::Manifest</code><br/>.jarkdown-manifest.json"]
        FRESH["<code>freshness::plan</code><br/>ADR-0001 decision"]
        FCACHE["<code>field_cache::FieldMetadataCache</code><br/>XDG · 24h TTL"]
        CFG["<code>config::ConfigManager</code><br/>.jarkdown.toml"]
        ATT["<code>attachment::AttachmentHandler</code><br/>download + dedupe"]
    end

    JIRA[(Jira Cloud<br/>REST API v3)]
    FS[(Filesystem<br/><code>{KEY}/{KEY}.md</code>)]

    CLI --> LIB
    CLI --> EXPORT
    CLI --> BULK
    CLI --> HIER
    LIB --> EXPORT

    BULK --> EXPORT
    BULK --> FRESH
    BULK --> MAN
    HIER --> EXSEAM
    EXSEAM --> EXPORT

    EXPORT --> CLIENT
    EXPORT --> ATT
    EXPORT --> CL
    EXPORT --> CFG
    EXPORT --> FCACHE
    EXPORT --> COMPOSE
    EXPORT --> FRESH

    CL --> CLIENT
    CLIENT --> RETRY
    CLIENT --> ISSUE
    ATT --> CLIENT

    COMPOSE --> SECTIONS
    SECTIONS --> ADF
    SECTIONS --> HTML
    SECTIONS --> ATTIDX
    SECTIONS --> CFR

    CLIENT -->|HTTPS| JIRA
    EXPORT -->|writes| FS
    CL -->|writes| FS
    ATT -->|writes| FS
```

**Layer rules to preserve:**

- The render layer is *pure* — `markdown::compose` borrows everything via
  `RenderContext`, takes no `&mut`, and performs no I/O. All mutable
  field-cache resolution happens up at the `export` seam.
- `jira_client` is pure transport above the HTTP boundary; typed parsing into
  `Issue` / `ChangelogEntry` / `IssueSearchResult` happens at that seam (see
  ADR-0002).
- `freshness::plan` is the *only* implementation of the incremental decision
  (see ADR-0001). The single-export CLI path and `BulkExporter` both consult it
  instead of re-deriving the rule.

## Single-issue export flow

What happens when you run `jarkdown-rs export PROJ-123`. Same flow is invoked
by the library entry point `export_issue` and by `BulkExporter` per task.

```mermaid
sequenceDiagram
    autonumber
    actor User
    participant CLI as main.rs<br/>handle_export
    participant Plan as freshness::plan<br/>(if --incremental)
    participant Exp as export::<br/>perform_export_with_options
    participant API as JiraApiClient
    participant Att as AttachmentHandler
    participant FC as FieldMetadataCache
    participant CL as changelog::write_artifacts
    participant Comp as markdown::compose
    participant FS as Filesystem

    User->>CLI: jarkdown-rs export PROJ-123 [flags]
    CLI->>API: new(domain, email, token)

    opt --incremental
        CLI->>API: fetch_issue(KEY)
        CLI->>Plan: plan(issue, manifest, ...)
        alt Skip
            Plan-->>CLI: ExportPlan::Skip
            CLI-->>User: "unchanged, skipping"
        else BackfillChangelogOnly
            Plan-->>CLI: ExportPlan::BackfillChangelogOnly
            CLI->>API: fetch_changelog(KEY)
            CLI->>CL: write_artifacts(...)
        else Full
            Plan-->>CLI: ExportPlan::Full
        end
    end

    CLI->>Exp: perform_export_with_options(...)
    Exp->>FS: create_dir_all(output_path)
    Exp->>API: fetch_issue(KEY)
    API-->>Exp: Issue (typed spine + raw Value)

    par Attachments (bounded by --attachment-concurrency)
        Exp->>Att: download_all_attachments(...)
        Att->>API: download_attachment(url) [N×]
        API-->>Att: bytes
        Att->>FS: write each file
        Att-->>Exp: Vec<DownloadedAttachment>
    and Field metadata
        Exp->>FC: is_stale()?
        opt stale
            Exp->>API: fetch_fields()
            Exp->>FC: save(fields)
        end
    end

    opt issuetype == "Epic"
        Exp->>API: search_jql("parent = KEY OR Epic Link = KEY")
    end

    opt --include-changelog
        Exp->>API: fetch_changelog(KEY) [paginated]
        Exp->>CL: write_artifacts → {KEY}.changelog.md
    end

    Exp->>Comp: compose(&RenderContext)
    Comp-->>Exp: String (markdown body)
    opt --include-json
        Exp->>FS: write {KEY}.json (raw payload)
    end
    Exp->>FS: write {KEY}.md
    Exp-->>CLI: PathBuf
    CLI-->>User: success summary
```

## Bulk concurrency model

`BulkExporter` fans work out across N concurrent issue tasks using a tokio
`Semaphore`. Each task internally runs the same `perform_export_with_options`
workflow shown above, with its own `--attachment-concurrency` budget — so the
effective parallelism is `concurrency × attachment_concurrency`.

```mermaid
flowchart LR
    keys["issue_keys: Vec&lt;String&gt;"] --> stream{{"stream::iter<br/>buffer_unordered"}}

    subgraph sem["Semaphore(concurrency=N)"]
        T1["task: KEY-1<br/>perform_export_with_options"]
        T2["task: KEY-2<br/>perform_export_with_options"]
        T3["task: KEY-3<br/>perform_export_with_options"]
        Tn["task: KEY-…"]
    end

    stream --> T1
    stream --> T2
    stream --> T3
    stream --> Tn

    T1 --> tout1{"time::timeout<br/>issue_timeout_seconds"}
    T2 --> tout2{"time::timeout"}
    T3 --> tout3{"time::timeout"}
    Tn --> toutn{"time::timeout"}

    tout1 --> R[(ExportResult)]
    tout2 --> R
    tout3 --> R
    toutn --> R

    R --> split{success?}
    split -->|true| succ["successes: Vec&lt;ExportResult&gt;"]
    split -->|false| fail["failures: Vec&lt;ExportResult&gt;"]

    succ --> idx["write_index_md → index.md"]
    fail --> idx
    succ --> manup["Manifest.record<br/>(if --incremental)"]
```

Per-task incremental short-circuit lives at the top of each task and consults
`freshness::plan` — unchanged issues never enter the full workflow.

## Hierarchy traversal

`--hierarchy` builds an `IssueNode` tree and writes each issue under its
parent's directory. The seam (`IssueExporter` trait) lets the production
implementation delegate to `perform_export_with_options` while tests can
substitute a recording fake.

```mermaid
flowchart TB
    start([export_hierarchy: root_key])
    epic{Resolve<br/>'Epic Link'<br/>field id}
    build["build_tree(key, dir, depth)"]
    cycle{"key in<br/>visited?"}
    cap{"depth ≥ max_depth<br/>OR<br/>count ≥ max_issues?"}
    exp["IssueExporter.export(key, dir)<br/>→ perform_export_with_options"]
    fetch["JiraApiClient.fetch_issue(key)<br/>for child discovery"]
    disc["Discover children:<br/>· subtasks<br/>· issuelinks (parent + 'is implemented by')<br/>· JQL: parent=K OR EpicLink=K"]
    dedup["Dedup + preserve order"]
    recurse["recurse into each child<br/>(depth+1, dir/key)"]
    leaf[("IssueNode<br/>{ key, summary, type, children }")]
    index["render_index → index.md"]

    start --> epic --> build --> cycle
    cycle -->|yes| leaf
    cycle -->|no| exp --> fetch --> cap
    cap -->|yes| leaf
    cap -->|no| disc --> dedup --> recurse --> leaf
    leaf --> index
```

## Incremental freshness decision (ADR-0001)

The `--incremental` skip rule is implemented in exactly one place. Both the
single-export CLI handler and `BulkExporter` call `freshness::plan` and act on
its `ExportPlan` instead of re-deriving the rule.

```mermaid
stateDiagram-v2
    [*] --> CheckManifest
    CheckManifest --> Full: manifest.is_stale(key, updated)
    CheckManifest --> CheckChangelogFlag: timestamps match

    CheckChangelogFlag --> Skip: --include-changelog OFF
    CheckChangelogFlag --> CheckArtifact: --include-changelog ON

    CheckArtifact --> Skip: {KEY}.changelog.md exists
    CheckArtifact --> BackfillChangelogOnly: file missing

    Skip --> [*]: no I/O
    BackfillChangelogOnly --> [*]: fetch + write changelog only<br/>(issue payload not re-fetched)
    Full --> [*]: standard export workflow
```

## Render pipeline (pure)

`markdown::compose` builds a Markdown file body from a borrowed
`RenderContext`. The context is constructed exactly once at the `export` seam,
with all mutable work (HTTP, field-cache resolution, attachment downloads)
already done. Section order is load-bearing for `--strict-md` byte-identity
against the baseline.

```mermaid
flowchart LR
    subgraph build["Built at export seam (mutable work done here)"]
        ISS["Issue<br/>(typed + raw)"]
        DLA["Vec&lt;DownloadedAttachment&gt;"]
        SKA["skipped_attachments<br/>(--no-attachments)"]
        CFM["CustomFieldMetadata<br/>names + schemas"]
        FF["FieldFilter"]
        CH["child_issues: &amp;[Value]"]
        CLS["Option&lt;ChangelogSummary&gt;"]
    end

    RC["RenderContext (borrowed)"]
    AI["AttachmentIndex::build(dl, sk)"]

    ISS --> RC
    DLA --> AI --> RC
    SKA --> AI
    SKA --> RC
    CFM --> RC
    FF --> RC
    CH --> RC
    CLS --> RC

    RC --> COMPOSE["compose(&amp;ctx) → String"]

    COMPOSE --> S1["frontmatter"]
    COMPOSE --> S2["title"]
    COMPOSE --> S3["description"]
    COMPOSE --> S4["environment"]
    COMPOSE --> S5["linked_issues"]
    COMPOSE --> S6["subtasks"]
    COMPOSE --> S7["child_issues"]
    COMPOSE --> S8["worklogs"]
    COMPOSE --> S9["custom_fields"]
    COMPOSE --> S10["comments"]
    COMPOSE --> S11["changelog (xref)"]
    COMPOSE --> S12["attachments"]

    S3 --> ADF[markdown::adf]
    S3 --> HTML[markdown::html]
    S10 --> ADF
    S10 --> HTML
    S9 --> CFR[custom_field::<br/>CustomFieldRenderer]
    S12 --> AI
```

**Why pure:** the render layer never touches `&mut FieldMetadataCache`, never
hits HTTP, never writes files. This is what makes `compose` trivially
reproducible — given the same `RenderContext`, the same bytes come out. Any
future caller (a different output format, a test fixture, an in-memory
preview) can build a `RenderContext` and reuse the entire composer.

## Typed Issue + raw payload duality (ADR-0002)

`JiraApiClient::fetch_issue` parses Jira's JSON into a typed `Issue` but
*retains the original `Value`* in `Issue.raw`. The typed spine drives logic;
`raw` drives `--include-json` byte-identity and custom-field lookup. Deleting
`raw` looks like duplication — it is not.

```mermaid
flowchart LR
    JIRA[(Jira API<br/>raw JSON)]
    PARSE["Issue::from_value(value)"]

    JIRA --> PARSE

    subgraph issue["Issue (one struct, two views)"]
        TYPED["Typed spine (~12 fields)<br/>key · summary · updated<br/>issuetype · status · priority<br/>project · assignee · ...<br/><b>parse failure here = hard error</b>"]
        RAW["raw: serde_json::Value<br/>(untouched)<br/><b>~30 standard + N custom fields</b>"]
    end

    PARSE --> TYPED
    PARSE --> RAW

    TYPED --> LOGIC["Business logic:<br/>· hierarchy traversal<br/>· field filtering<br/>· markdown sections<br/>· freshness check"]

    RAW --> JSONOUT["{KEY}.json<br/>(--include-json)<br/><b>byte-identical to Jira</b>"]
    RAW --> CF["custom field reads<br/>via Issue.field(name)"]
    RAW --> CHILD["unmodeled<br/>standard fields"]
```

**Concretely:** if `Jira returns "updated": 12345` (wrong type) we fail fast at
the HTTP seam — schema drift on the narrow stable spine is a real bug. But if
`customfield_98765` returns an unfamiliar ADF shape, nothing fails; that data
lives only in `raw` and is parsed only on demand by the section renderers.

## Attachment download pipeline

Two-phase: filename resolution is synchronous (no races); downloads run
concurrently bounded by `--attachment-concurrency`. `--no-attachments` skips
phase 2 entirely, but the renderer still emits each attachment's original
Jira URL via `AttachmentIndex::skipped_*` so links never break.

```mermaid
flowchart LR
    A["issue.attachments<br/>Vec&lt;Value&gt;"]
    P1["Phase 1 (sync):<br/>resolve_filename per attachment<br/>HashSet&lt;String&gt; tracks used names<br/>conflicts → 'name_1.ext', 'name_2.ext', ..."]
    P2["Phase 2 (async):<br/>stream::buffer_unordered(N)<br/>download_attachment_to(path)"]
    OUT["Vec&lt;DownloadedAttachment&gt;<br/>{ filename, original_filename,<br/>mime_type, path }"]
    SKIP{"--no-attachments?"}
    AI["AttachmentIndex::build(<br/>  downloaded, skipped)"]

    A --> SKIP
    SKIP -->|no| P1 --> P2 --> OUT --> AI
    SKIP -->|yes| AI
```

The index dual-keys downloaded attachments (by `id`, by `original_filename`,
and by the conflict-resolved local `filename`) and single-keys skipped ones
(by `id` and `filename`). Lookups in `markdown::adf` / `markdown::sections`
normalize via `.trim().to_lowercase()` and try id first, then name hint.

## Where to look in the source

| Concern | File |
|---|---|
| CLI parsing | `src/cli.rs`, `src/main.rs` |
| Library entry point | `src/lib.rs` |
| Single-export orchestration | `src/export.rs` |
| Bulk concurrency | `src/bulk.rs` |
| Hierarchy traversal | `src/hierarchy.rs`, `src/exporter.rs` |
| HTTP transport | `src/jira_client.rs`, `src/retry.rs` |
| Typed issue model | `src/issue.rs` |
| Incremental decision | `src/freshness.rs`, `src/manifest.rs` |
| Render pipeline (pure) | `src/markdown/{mod,sections,adf,html,attachments}.rs` |
| Custom field rendering | `src/custom_field.rs` |
| Changelog rendering | `src/changelog.rs` |
| Field metadata cache | `src/field_cache.rs` |
| User configuration | `src/config.rs` |
| Attachment downloads | `src/attachment.rs` |
| Error type | `src/error.rs` |
