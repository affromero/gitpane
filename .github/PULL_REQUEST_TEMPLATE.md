<!--
Please open an issue first (bug report or feature request) so the change is
tracked and searchable, then reference it below. For trivial fixes (typos,
doc tweaks) skip the issue.
-->

Closes #<issue number>

## Type

- [ ] Bug fix
- [ ] Feature
- [ ] Documentation
- [ ] Refactor or maintenance
- [ ] Dependency update

## Summary

One or two sentences describing what changes for users.

## Problem

What is wrong and why it matters. Include repro steps or observed output for bugs.

- [ ] I reviewed the issue body and follow-up comments, and addressed each
      request or listed what remains.
- [ ] I reviewed every path in Files changed. Each file belongs in this PR,
      and the Fix section explains any new top-level paths or generated files.

## Fix

What the change does and why this approach.

## Testing

How the change was verified, including new or updated tests and relevant CI
checks.
For changes likely to affect TUI runtime, include the before-and-after
benchmark report and note any regressions.

- [ ] `just ci` passes
- [ ] Tests were added or updated where behavior changed
- [ ] Documentation was updated where needed

## Demo

Embed GIFs for every change that can be demonstrated visually, including bug
fixes. Show each affected interface, such as the TUI and VS Code. If existing
behavior changes, show the same steps before and after the change. For new
behavior, show it in use. If a GIF does not apply, briefly explain why.

Attach review-only media to the PR or commit it temporarily and embed it using
a URL pinned to the commit containing the file. Before merging, remove
review-only files in a follow-up commit on the PR branch and verify that the
embeds still load. Use a merge commit to preserve the media commit in history.
Keep media used by the product or maintained documentation.

## Maintainer review

- [ ] Read the linked issue and discussion, then review the complete diff
      against the requested scope.
- [ ] Reproduce the reported behavior or feature in a representative
      environment and verify the proposed fix at the commit being merged.
- [ ] Confirm the required checks passed for that commit. Repeat affected
      verification after conflict resolution or other changes.
- [ ] Confirm the GIFs demonstrate the change in each affected interface, or
      that the PR explains why a GIF does not apply.
- [ ] Confirm review-only media is absent from the final diff and still loads
      in the PR. If it already landed on the base branch, remove it in a
      follow-up change.

## Review assistance

If review identifies changes, how would you like us to proceed?

- [ ] Please use our AI agents to review this PR and prepare any requested
      follow-up changes. I will review the result.
- [ ] I will make any requested changes myself. Please leave review comments
      only.
- [ ] No preference.

If you selected AI-agent assistance, describe any constraints, acceptance
criteria, or files that should be included in the follow-up work:

<!--
Leave this section blank if you selected another option.
-->
