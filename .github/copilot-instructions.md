# Copilot instructions

## Commits and pull requests

This repository uses [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/). Every commit
message and every pull request title must follow it; the `Conventional commits` workflow
(`.github/workflows/conventional-commits.yml`) fails PRs that don't.

```
<type>(<optional scope>)!: <description>
```

- Allowed types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`.
- Use the imperative mood, lower-case description, no trailing full stop, e.g. `fix: hide indicator behind full-screen apps`.
- Mark breaking changes with `!` after the type/scope and a `BREAKING CHANGE:` footer.
- Name branches with a matching prefix (`feat/…`, `fix/…`, `docs/…`, `ci/…`) so the labeler can categorise release notes.
- `main` is protected: changes go through a pull request, never a direct push.
