# Agent instructions

<!-- project-ops:start -->
## Project tracking

Managed by project-ops (github.com/mizaimao/project-ops). Edit it there, not here; changes
made here are overwritten.

This repo's own rules elsewhere in this file come first. Where they are stricter (for
example "never push unless asked" or "never commit on main"), follow them and tell Frank
what is waiting instead. Frank's general rules are in `~/Projects/knowledgebase/rules.md`.

1. Session start: read `STATUS.md`, then run `gh issue list --label agent-ready`.
2. During work: if you hit an unrelated problem, file an issue and go back to the current
   task. Only file real bugs or blockers, or work that outlives this session. At most about
   3 new issues per session, and no speculative "consider refactoring" tickets.
3. Epics (label `needs-breakdown`): split them into session-sized sub-issues with acceptance
   criteria (GitHub sub-issues; if that fails, a task list in the epic's body). Label each
   one `needs-decision` for Frank's approval, and don't start implementing until he relabels
   it `agent-ready`.
4. Anything that needs Frank's judgment: label it `needs-decision` and put a clear question
   in the issue body.
5. Closing work: open PRs that close issues with "Fixes #N". Never push directly to the
   default branch.
6. Session end: update `STATUS.md`, both the frontmatter (`phase`, `next`, `blocked_on`) and
   "Where we left off". Reference issue numbers instead of copying issue content.

Labels: type `bug`, `feature`, `chore`; readiness `agent-ready`, `needs-decision`, `blocked`,
`needs-breakdown`; size `small`, `big`.
<!-- project-ops:end -->
