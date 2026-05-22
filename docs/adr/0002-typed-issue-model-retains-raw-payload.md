# Typed Issue model retains the raw Jira payload

`fetch_issue` and `search_jql` parse Jira's JSON into typed structs (`Issue`, `IssueSearchResult`) instead of returning `serde_json::Value`. One non-obvious choice: the typed `Issue` retains the original `Value` it was parsed from, in a `raw` field, rather than fully replacing it.

## Decision: keep `raw: Value` alongside the typed fields

`--include-json` writes `{KEY}.json` by serializing the exact payload Jira returned. A hand-written struct cannot reproduce that byte-for-byte — Jira returns ~30 standard fields plus instance-specific custom fields in an order no Rust struct will match, and the typed `Issue` deliberately models only ~12 of them. Serializing a typed `Issue` directly would silently drop every unmodeled field and reorder the rest.

So the typed `Issue` carries both: the ~12 typed fields used by the rest of the codebase, and `raw`, the untouched `Value`. The `--include-json` writer serializes `raw`; every other consumer uses the typed fields. `raw` is also the escape hatch for unmodeled data — custom fields and the ~19 display-only standard fields are reached via a `field(name)` helper over `raw["fields"]`, not modeled as struct fields.

**Do not remove `raw` as redundant.** It looks like duplication — it is not. Deleting it breaks byte-identical `{KEY}.json` output and removes the only access path to custom fields and unmodeled standard fields.

The typed view is parsed strictly: a malformed value in any of the ~12 typed fields is a hard error surfaced from the HTTP layer. This is safe precisely because the typed set is narrow and stable — issue key, summary, status, issuetype, updated, and similar fields whose shape never varies. Volatile data — custom fields, ADF bodies — lives only in `raw` and is never parsed, so it cannot trigger a parse failure.

## Considered alternatives

- **Full replacement — the typed `Issue` supplants the `Value`** — rejected. Reproducing byte-identical `{KEY}.json` would require `#[serde(flatten)]` catch-all maps for every unmodeled field and still risk field-ordering drift. Heavy machinery to preserve output that retaining `raw` preserves for free.
- **Split return — `fetch_issue` returns `(Issue, Value)`** — rejected. The raw `Value` would have to be threaded as a second value through every call site that might write JSON, for no benefit over carrying it inside `Issue`.
- **Lenient parsing — malformed typed fields degrade to `None`** — rejected. It re-introduces the silent, late-surfacing schema drift the typed model was introduced to eliminate. Strict parsing of a narrow, stable field set fails loud and early at the HTTP seam instead.
