# Skills

Tau discovers Markdown skills at session start, advertises only the small set that should be immediately visible, and lets the agent discover or load the rest with the `skill` tool.


## Discovery

Tau scans, in priority order:

1. `<cwd>/.agents/skills`
2. `<cwd>/.agents.local/skills`
3. `~/.agents/skills`
4. `~/.agents.local/skills`
5. `~/.config/agents/skills`
6. `~/.config/agents.local/skills`

The first skill with a given name wins. Later duplicates are ignored and reported as collisions.

Preferred layout:

```text
.agents/skills/<skill-name>/SKILL.md
```

The frontmatter fields Tau reads are:

- `name`: Optional. Defaults to the parent directory name. Must be lowercase ASCII letters, digits, and hyphens only.
- `description`: Required. Used in prompt advertisements and search results.
- `advertise`: Optional. `true`, `True`, `TRUE`, and `1` force prompt advertisement. Any other explicit value keeps the skill hidden from the initial prompt.

Project-scoped skills default to advertised. User-scoped skills default to hidden until searched. `advertise:` overrides the scope default.


## Prompt advertisement

Advertised skills appear in `<available_skills>` with only name and description. Tau does not include the skill body until the agent calls `skill`.

This keeps normal session context small while still surfacing project-local instructions that are likely relevant immediately.


## The `skill` tool

The agent calls `skill` with a `query` string or an array of query strings:

```json
{ "query": ["rust", "style"] }
```

Tau trims, lowercases, and deduplicates query terms, then matches them against skill names and descriptions. Hits are merged and sorted by `hit_count` descending then by name. By default, Tau does not read skill bodies during search; `search_content: true` also searches body text.

If the query is unambiguous, Tau returns the full skill body with frontmatter stripped:

- exactly one matching skill was found; or
- the query has one term and one match has exactly that skill name, even if other skills also matched.

Otherwise Tau returns matching skill names, descriptions, and hit counts. This supports large, inter-connected skill libraries: agents can cheaply search broad terms, follow names mentioned by other skills, and load full bodies only when needed instead of spending context on every possible instruction up front.
