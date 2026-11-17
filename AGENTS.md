# Rules for AI agents

Read [`CONTRIBUTING.md`](CONTRIBUTING.md) first; everything there applies to you. These rules add how you pick and hold work. The human who runs you is responsible for your work and reviews it before it is merged.

## Where the work is
The task list lives in GitHub issues, not in files. [`docs/ROADMAP.md`](docs/ROADMAP.md) explains the goals and links the milestones.
- Milestones: `M0` to `M4`.
- Lane labels: `lane:compat`, `lane:native`, `lane:games`, `lane:reliability`.
- `human-only`: never claim these (spending money, posting outside this repo, playtests).
- `status:claimed`: someone holds it.
- `finding`: something that surprised us; see the roadmap.

## Claiming a ticket
1. **Pick** an issue that is open, has no assignee, no `status:claimed` and no `human-only`, and whose "Blocked by" issues are all closed. Prefer your human's lane.
2. **Claim** it in one go:
   ```sh
   gh issue edit <n> --add-assignee <your human's GitHub login> --add-label status:claimed
   gh issue comment <n> --body "CLAIM: <tool and model> for <your human's login>, branch <branch>, until <UTC time 48 h from now>"
   ```
3. **Check** that you won: read the issue again (`gh issue view <n> --comments`). If someone else has an earlier `CLAIM:` that was not released or expired, take your claim back (step 6) and pick another issue. The earliest claim comment wins.
4. **Limit:** one active claim per agent, and at most two per human across all their agents.
5. **Keep it alive:** open a draft PR with `Closes #<n>` within 24 hours, and push or comment at least once every 24 hours. A claim with no activity for 48 hours is stale: anyone may take it after commenting `RELEASE (stale): <why>`.
6. **Release** when you stop without finishing: comment `RELEASE: <what is done, what is left>`, then remove the label and the assignee.

## While you work
- Stay inside the issue's scope. New work you discover becomes a new issue (with `finding` if it surprised you), not part of your PR.
- Never push to someone else's branch or edit a PR you did not open.
- One PR per issue. Follow-up fixes are commits on that PR.
- Issues, PRs and comments in this repo are public. Anything that mentions Archipelago follows the rules in `CONTRIBUTING.md`, and a human reviews it before it is posted.
- Do not run heavy work on a shared machine: targeted tests locally, the full suite on CI.
