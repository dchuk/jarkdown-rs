# Architecture

This document describes how jarkdown-rs is layered and how data flows through
the codebase. It complements:

- `CONTEXT.md` — domain glossary.
- `docs/manifest-v2.md` — manifest v2 cache format and diagnostics.
- `docs/adr/0001-changelog-export.md` — why changelog artifacts are siblings
  with one-time incremental backfill semantics.
- `docs/adr/0002-typed-issue-model-retains-raw-payload.md` — why `Issue` keeps
  both typed fields and the raw Jira payload.
- `docs/adr/0003-incremental-validation-uses-manifest-metadata.md` — why warm
  incremental paths use validation metadata instead of full Issue fetches.
- `docs/adr/0004-child-aware-incremental-manifest.md` — why hierarchy cache
  state is graph-backed.

All diagrams are MermaidJS and render natively on GitHub and most modern
Markdown viewers.

## Overall Architecture

The CLI is a thin shell over the reusable library surface. Network transport,
domain parsing, rendering, cache planning, and filesystem writes stay separated
so tests can exercise behavior with local scripted HTTP servers and fake
exporters.

```mermaid
flowchart TB
    subgraph entry[Entry surface]
        CLI["CLI bin<br/><code>jarkdown-rs</code><br/>(main.rs, cli.rs)"]
        LIB["Library API<br/>(lib.rs:<br/><code>export_issue</code>, <code>ExportOptions</code>)"]
    end

    subgraph orch[Orchestration]
        EXPORT["<code>export::perform_export_with_options</code><br/>single Issue workflow"]
        BULK["<code>bulk::BulkExporter</code><br/>concurrent flat export"]
        HIER["<code>hierarchy::HierarchyExporter</code><br/>recursive traversal"]
        PLANNER["main.rs hierarchy warm planner<br/>validation, skip, refresh"]
        EXSEAM["<code>exporter::IssueExporter</code><br/>test seam"]
    end

    subgraph domain[HTTP and domain]
        CLIENT["<code>jira_client::JiraApiClient</code><br/>REST, auth, pagination"]
        ISSUE["<code>issue::Issue</code><br/>typed spine + raw payload"]
        CL["<code>changelog::*</code><br/>render + write"]
        RETRY["<code>retry::retry_with_backoff</code>"]
    end

    subgraph state[Persistence and planning]
        MAN["<code>manifest::Manifest</code><br/>manifest v2 graph cache"]
        FRESH["<code>freshness::plan_metadata</code><br/>freshness decision"]
        FCACHE["<code>field_cache::FieldMetadataCache</code><br/>XDG, 24h TTL"]
        CFG["<code>config::ConfigManager</code><br/>.jarkdown.toml"]
        ATT["<code>attachment::AttachmentHandler</code><br/>download + dedupe"]
    end

    subgraph render[Pure rendering]
        COMPOSE["<code>markdown::compose(&RenderContext)</code>"]
        SECTIONS["<code>markdown::sections</code>"]
        ADF["<code>markdown::adf</code>"]
        HTML["<code>markdown::html</code>"]
        ATTIDX["<code>markdown::attachments::AttachmentIndex</code>"]
        CFR["<code>custom_field::CustomFieldRenderer</code>"]
    end

    JIRA[(Jira Cloud REST API v3)]
    FS[(Filesystem artifacts)]

    CLI --> LIB
    CLI --> EXPORT
    CLI --> BULK
    CLI --> PLANNER
    PLANNER --> HIER
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
    PLANNER --> CLIENT
    PLANNER --> FRESH
    PLANNER --> MAN

    CLIENT --> RETRY
    CLIENT --> ISSUE
    CL --> CLIENT
    ATT --> CLIENT

    COMPOSE --> SECTIONS
    SECTIONS --> ADF
    SECTIONS --> HTML
    SECTIONS --> ATTIDX
    SECTIONS --> CFR

    CLIENT -->|HTTPS| JIRA
    EXPORT -->|writes| FS
    HIER -->|writes indexes| FS
    CL -->|writes sidecars| FS
    ATT -->|writes binaries| FS
    MAN -->|writes cache| FS
```

Layer rules to preserve:

- The render layer is pure. `markdown::compose` borrows a `RenderContext`, takes
  no mutable dependencies, performs no I/O, and makes no HTTP calls.
- `jira_client` owns transport and pagination. Typed parsing into `Issue`,
  `ChangelogEntry`, `IssueSearchResult`, and `ValidationIssue` happens at that
  seam.
- Incremental decisions go through `freshness::plan_metadata` or its
  compatibility wrapper `freshness::plan`; callers should not re-derive
  timestamp/artifact/fingerprint rules inline.
- Manifest v2 is the source of cache truth for incremental state, hierarchy
  edges, requested roots, artifact paths, evictions, and root snapshots.

## Single-Issue Export Flow

`jarkdown-rs export PROJ-123` has two phases when `--incremental` is enabled:
cheap validation/planning first, then full export only when required. The
library entry point `export_issue` runs the full export workflow directly
because it does not expose every CLI cache-planning flag.

```mermaid
sequenceDiagram
    autonumber
    actor User
    participant CLI as main.rs handle_export
    participant API as JiraApiClient
    participant MAN as Manifest
    participant Plan as freshness::plan_metadata
    participant Exp as perform_export_with_options
    participant CL as changelog::write_artifacts
    participant FS as Filesystem

    User->>CLI: jarkdown-rs export PROJ-123 [flags]
    CLI->>API: new(domain, email, token)

    opt --incremental and not --force
        CLI->>MAN: load_from_path(manifest path)
        CLI->>API: validate_issue_keys([KEY])
        alt validation returns KEY
            CLI->>Plan: plan_metadata(KEY, updated, manifest, options, output path)
            alt Skip
                Plan-->>CLI: ExportPlan::Skip
                CLI->>MAN: record validation metadata
                CLI->>MAN: save_to_path(...)
                CLI-->>User: unchanged
            else BackfillChangelogOnly
                Plan-->>CLI: ExportPlan::BackfillChangelogOnly
                CLI->>API: fetch_changelog(KEY)
                CLI->>CL: write_artifacts(...)
                CLI->>MAN: record validation metadata + new fingerprint
                CLI->>MAN: save_to_path(...)
                CLI-->>User: repaired sidecar
            else Full
                Plan-->>CLI: ExportPlan::Full
            end
        else successful validation omits active KEY
            CLI->>MAN: evict(KEY, not_returned_by_validation_search)
            CLI->>MAN: save_to_path(...)
            CLI-->>User: cache entry evicted
        else validation request fails
            CLI-->>CLI: warn and fall through to full export
        end
    end

    CLI->>Exp: perform_export_with_options(...)
    Exp->>FS: create_dir_all(output_path)
    Exp->>API: fetch_issue(KEY)
    API-->>Exp: Issue (typed spine + raw payload)
    Exp->>API: fetch fields / child issues / changelog as flags require
    Exp->>FS: write attachments, JSON, changelog sidecars, Markdown
    Exp-->>CLI: PathBuf
    opt --incremental
        CLI->>API: fetch_issue(KEY) for manifest metadata
        CLI->>MAN: record_issue_with_fingerprint(...)
        CLI->>MAN: save_to_path(...)
    end
    CLI-->>User: success summary
```

## Bulk and Query Export

Bulk and query flat exports use one validation pass per invocation when
`--incremental` is enabled and `--force` is not set. Each requested key then
plans independently against the shared validation map.

```mermaid
flowchart LR
    keys["requested keys"] --> validate{"--incremental<br/>and not --force?"}
    validate -->|yes| jira["validate_issue_keys(keys)<br/>updated + display metadata"]
    validate -->|no| stream
    jira --> stream{{"stream::iter<br/>buffer_unordered(concurrency)"}}

    subgraph task["per-key task"]
        plan["freshness::plan_metadata"]
        skip["Skip<br/>record metadata"]
        backfill["BackfillChangelogOnly<br/>fetch changelog only"]
        evict["Validation omitted active key<br/>evict tombstone"]
        full["Full export<br/>perform_export_with_options"]
    end

    stream --> plan
    plan --> skip
    plan --> backfill
    plan --> evict
    plan --> full
    full --> result["ExportResult"]
    skip --> result
    backfill --> result
    evict --> result
    result --> manifest["merge manifest updates<br/>save_to_path"]
    result --> index["bulk/query index.md"]
```

Per-task full export still uses the same attachment concurrency and timeout
rules as non-incremental export. `query --keys-only` stops after printing keys;
it does not write artifacts or manifests.

## Hierarchy Export and Layouts

`--hierarchy` builds an `IssueNode` tree. The production `IssueExporter`
delegates each Issue artifact write to `perform_export_with_options`, while
tests can substitute a fake exporter. Layout determines where artifacts are
written:

- `corpus`: one canonical directory per Issue under the output root, plus a
  `{ROOT}.hierarchy.md` snapshot.
- `nested`: tree-shaped directories for browsing, plus `index.md`.

`--incremental --hierarchy` defaults to `corpus` because it best matches
shared-cache semantics. Non-incremental hierarchy export keeps the legacy nested
default.

```mermaid
flowchart TB
    start([export_hierarchy root])
    build["build_tree(key, dir, depth)"]
    stack{"key in recursion stack?"}
    emitted{"key already emitted<br/>outside current stack?"}
    export["IssueExporter.export(key, dir)<br/>writes current artifact path"]
    fetch["fetch_issue(key)<br/>for child discovery"]
    cap{"depth >= max_depth<br/>or count >= max_issues?"}
    discover["discover children:<br/>subtasks, parent links,<br/>JPD delivery links, Epic Link JQL"]
    recurse["recurse children"]
    node["IssueNode"]
    snapshot["render root snapshot/index"]

    start --> build --> stack
    stack -->|yes cycle| node
    stack -->|no| emitted
    emitted -->|yes shared child| export --> node
    emitted -->|no| export --> fetch --> cap
    cap -->|yes truncated| node
    cap -->|no| discover --> recurse --> node
    node --> snapshot
```

The traversal distinguishes recursion-stack cycle prevention from non-cyclic
shared-child revisits. A shared child can be written at multiple nested paths and
recorded under multiple parent edges without duplicating child discovery.

## Child-Aware Incremental Hierarchy

Warm hierarchy planning validates the current Requested Root plus its active
cached descendants. Successful validation omissions evict only the omitted
active keys and continue planning the remaining validated descendants. Failed
validation requests evict nothing.

```mermaid
flowchart TB
    load["load manifest v2"]
    tree{"cached root snapshot exists?"}
    keys["active_hierarchy_keys(root)"]
    validate["validate current root + active descendants"]
    missing["evict validation omissions"]
    plan["plan_metadata for each validated key/path"]
    allskip{"all Skip?"}
    changed{"descendant changed<br/>or sidecar missing?"}
    rootdirty{"root needs Full<br/>or Backfill?"}
    refresh["refresh changed descendants only"]
    cached["return cached_hierarchy_tree(root)"]
    full["fall back to full hierarchy traversal"]

    load --> tree
    tree -->|no| full
    tree -->|yes| keys --> validate --> missing --> plan
    plan --> allskip
    allskip -->|yes| cached
    allskip -->|no| rootdirty
    rootdirty -->|yes| full
    rootdirty -->|no| changed --> refresh --> cached
```

Changed leaf descendants are re-exported directly. Changed descendants with
active children are refreshed as bounded subtrees. The subtree depth is computed
from the remaining depth below active Requested-Root paths, taking the maximum
remaining depth across paths. In nested layout, refresh writes a changed shared
Issue to every active artifact path recorded for it.

Root snapshots are traversal snapshots, not live views. Descendant-only refresh
does not rebuild unchanged ancestor snapshots.

## Freshness Decision

`freshness::plan_metadata` combines manifest state, parsed Jira timestamps,
artifact presence, and content-visible option fingerprints.

```mermaid
stateDiagram-v2
    [*] --> ActiveEntry
    ActiveEntry --> Full: no active entry
    ActiveEntry --> Timestamp

    Timestamp --> Full: incoming newer
    Timestamp --> Options: equal or incoming older
    Timestamp --> StringFallback: parse failed
    StringFallback --> Full: strings differ
    StringFallback --> Options: strings equal

    Options --> Full: fingerprint changed
    Options --> MainMd: same fingerprint
    Options --> MainMd: only include_changelog changed

    MainMd --> Full: {KEY}.md missing
    MainMd --> Json: main Markdown exists
    Json --> Full: --include-json and {KEY}.json missing
    Json --> Changelog: JSON ok
    Changelog --> BackfillChangelogOnly: --include-changelog and changelog sidecar missing
    Changelog --> Skip: required artifacts present

    Skip --> [*]: no artifact writes
    BackfillChangelogOnly --> [*]: fetch/write changelog only
    Full --> [*]: standard export workflow
```

Timestamp parsing accepts RFC3339 and Jira's `%Y-%m-%dT%H:%M:%S%.f%z` shape. An
older parsed validation timestamp warns and does not regress the stored manifest
timestamp.

## Render Pipeline

`markdown::compose` builds Markdown from a borrowed `RenderContext`. All mutable
work (HTTP, field-cache resolution, attachment downloads, changelog fetches) is
complete before the context is constructed.

```mermaid
flowchart LR
    subgraph build["Built at export seam"]
        ISS["Issue"]
        DLA["DownloadedAttachment list"]
        SKA["skipped attachments"]
        CFM["field metadata"]
        FF["field filters"]
        CH["child Issues"]
        CLS["optional changelog summary"]
    end

    ISS --> RC["RenderContext"]
    DLA --> AI["AttachmentIndex"] --> RC
    SKA --> AI
    CFM --> RC
    FF --> RC
    CH --> RC
    CLS --> RC
    RC --> COMPOSE["markdown::compose"]
    COMPOSE --> MD["{KEY}.md"]
```

The render layer never touches `FieldMetadataCache`, HTTP clients, or the
filesystem. Given the same `RenderContext`, it should produce the same bytes.

## Typed Issue + Raw Payload Duality

`JiraApiClient::fetch_issue` parses Jira JSON into a typed `Issue` and keeps the
original `serde_json::Value` in `Issue.raw`.

```mermaid
flowchart LR
    JIRA[(Jira raw JSON)] --> PARSE["Issue::from_value"]
    PARSE --> TYPED["typed spine<br/>key, summary, updated,<br/>type, status, project, ..."]
    PARSE --> RAW["raw payload"]
    TYPED --> LOGIC["business logic<br/>freshness, hierarchy,<br/>render sections"]
    RAW --> JSON["{KEY}.json<br/>(--include-json)"]
    RAW --> CUSTOM["custom field reads"]
```

Wrong types in the narrow stable spine are hard errors. Unknown or changing
custom-field shapes remain in `raw` and are interpreted only by field renderers
that opt into them.

## Attachment Download Pipeline

Filename resolution is synchronous and deterministic; downloads run
concurrently bounded by `--attachment-concurrency`. `--no-attachments` skips
binary downloads while preserving source Jira URLs in rendered Markdown.

```mermaid
flowchart LR
    A["issue.attachments"] --> skip{"--no-attachments?"}
    skip -->|yes| index["AttachmentIndex<br/>source URLs"]
    skip -->|no| names["resolve filenames<br/>dedupe conflicts"]
    names --> downloads["buffer_unordered(N)<br/>download_attachment"]
    downloads --> files["write files"]
    files --> index["AttachmentIndex"]
```

## Where to Look in the Source

| Concern | File |
|---|---|
| CLI parsing | `src/cli.rs`, `src/main.rs` |
| Library entry point | `src/lib.rs` |
| Single-export workflow | `src/export.rs` |
| Flat bulk/query orchestration | `src/bulk.rs`, `src/main.rs` |
| Hierarchy traversal | `src/hierarchy.rs`, `src/exporter.rs` |
| Hierarchy warm planning | `src/main.rs` |
| HTTP transport | `src/jira_client.rs`, `src/retry.rs` |
| Typed Issue model | `src/issue.rs` |
| Incremental decision | `src/freshness.rs`, `src/manifest.rs` |
| Manifest v2 cache | `src/manifest.rs`, `docs/manifest-v2.md` |
| Render pipeline | `src/markdown/{mod,sections,adf,html,attachments}.rs` |
| Custom field rendering | `src/custom_field.rs` |
| Changelog rendering | `src/changelog.rs` |
| Field metadata cache | `src/field_cache.rs` |
| User configuration | `src/config.rs` |
| Attachment downloads | `src/attachment.rs` |
| Error type | `src/error.rs` |
