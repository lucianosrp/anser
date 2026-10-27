# knowledge/ — the agents' long-term memory

Durable notes about the user's finances and about where the app is going.
Agents (Claude Code, the MCP-connected assistant, subagents) read these
**before** analysing money or changing the dashboard, and update them after
learning something new. Think of it as a workspace memory, not documentation:
the code is documented in README.md / DEPLOY.md / DESIGN.md; this folder holds
what the code and the data can't say on their own.

**Privacy:** everything here except this README is gitignored. Keep this
file free of figures and personal facts; they belong in the ignored files. It never goes to
git history and is not shipped to the server by `scripts/deploy.sh`.

## Conventions

- One topic per file; **update in place, don't add a dated duplicate**.
- First line of each file: `*As of <data date>, updated <date>. …*`.
- Label every statement as **FACT** (from a query), **INFERENCE** (with a
  confidence), or **OPEN QUESTION** (for the owner). Once the owner answers a
  question, record the answer as a fact and note where it came from.
- Money in HKD unless native currency is clearer (then give both).
- The data is only as fresh as the last export. Check `data-quality.md` before
  you trust a recent month.
- `assessments/` holds point-in-time reviews, named `YYYY-MM-DD-<topic>.md`
  (`YYYY-MM-<topic>.md` when only the month is known)
  (dated by the data they describe). These are snapshots: **never edit an old
  one**; write a new dated file and note what changed since the last. The
  living files above them are the current truth.
- `ideas/` holds feature ideas for the app, one file per theme. When an idea
  ships, mark it done and link the commit. Don't delete it.

## Index

The file list lives in [INDEX.md](INDEX.md). It is gitignored because its
one-line summaries carry figures. Read it next, and add a row when you create a file.
