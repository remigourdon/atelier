# A carnet's folder is its record

Every other item lives in the database, but a carnet is recorded by its folder: any dated git repo directly under the carnet root is a carnet, and its tickets, whether it is closed, and its summary live in TOML front matter (`+++`) at the top of its README, which atelier commits when it edits it. The database keeps only what is local to this machine, the carnet's workspace and tab. Carnets are a history to browse and search years later, from atelier or plain tools like ripgrep, so the folder has to carry its own meaning: with the record in the database, regrouping left folder names stale, GitHub keys like `repo#12` could not be recorded outside it, and folders appeared or vanished from atelier only through `carnet add` and forgetting.

## Considered Options

- **One repo for all carnets, a branch per carnet, each checked out as a worktree.** It deletes the carnet item kind and gives closing for free (remove the worktree, keep the branch), but a closed carnet is no longer a folder on disk, which defeats browsing the history.
