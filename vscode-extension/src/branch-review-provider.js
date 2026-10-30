'use strict';

const path = require('node:path');
const { openGitlinkDiff } = require('./gitlink-diff');
const { createLiveGitSide } = require('./git-side-documents');
const { relativeFile } = require('./change-pager');

class BranchReviewProvider {
  constructor(tree, backend, git, options = {}) {
    this.tree = tree;
    this.backend = backend;
    this.git = git;
    this.vscode = options.vscode || require('vscode');
    this.links = options.links;
    this.generation = 0;
    this.disposed = false;
  }

  current(repo, generation, head, guard = () => true) {
    const current = this.tree.git.repositories.find(item => item.rootUri.toString() === repo.rootUri.toString());
    return guard() && !this.disposed && this.generation === generation && !!current && this.tree.isVisible(current) &&
      (!head || current.state?.HEAD?.commit === head);
  }

  async open(repo, guard = () => true, worktree) {
    const generation = ++this.generation;
    const token = repo && this.tree.repositoryToken(repo);
    const currentGuard = () => guard() && this.tree.isCurrentRepository(repo, token);
    if (!repo || !this.current(repo, generation, undefined, currentGuard)) {
      await this.vscode.window.showErrorMessage('Gitpane branch review: repository is no longer in the workspace.');
      return;
    }
    try {
      const result = await this.backend.branchReview(repo.rootUri.fsPath);
      if (!this.current(repo, generation, result?.head_oid, currentGuard)) throw new Error('Repository or HEAD changed; refresh and try again.');
      if (!result || typeof result.base_ref !== 'string' || !/^[a-f0-9]{40,64}$/.test(result.base_oid) ||
        !/^[a-f0-9]{40,64}$/.test(result.head_oid) || !Array.isArray(result.files)) {
        throw new Error('Gitpane returned an invalid branch review.');
      }
      const choices = result.files.map(file => {
        if (!file || !relativeFile(file.path) || !['A', 'D', 'M', 'R', '?'].includes(file.status) ||
          (file.status === 'R' && !relativeFile(file.old_path)) || typeof file.is_submodule !== 'boolean') {
          throw new Error('Gitpane returned an invalid branch review file.');
        }
        return { label: file.path, description: file.status, file };
      });
      if (!choices.length) {
        await this.vscode.window.showInformationMessage(`No committed changes since ${result.base_ref}.`);
        return;
      }
      const selected = await this.vscode.window.showQuickPick(choices, {
        placeHolder: `Review changes since ${result.base_ref}`,
        matchOnDescription: true
      });
      if (!selected || !choices.includes(selected)) return;
      if (!this.current(repo, generation, result.head_oid, currentGuard)) throw new Error('Repository or HEAD changed; refresh and try again.');
      const file = selected.file;
      if (file.is_submodule) {
        const current = () => this.tree.isCurrentRepository(repo, token) && this.tree.git.repositories.some(item =>
          item.rootUri.toString() === repo.rootUri.toString()) && (!worktree || worktree.current());
        const verify = async () => {
          if (!this.current(repo, generation, result.head_oid, currentGuard)) return false;
          const latest = await this.backend.branchReview(repo.rootUri.fsPath);
          if (!this.current(repo, generation, result.head_oid, currentGuard) || !latest ||
              latest.repo_identity !== result.repo_identity || latest.base_ref !== result.base_ref ||
              latest.base_oid !== result.base_oid || latest.head_oid !== result.head_oid || !Array.isArray(latest.files)) return false;
          return latest.files.some(item => item.path === file.path && item.status === file.status &&
            item.old_path === file.old_path && item.is_submodule === true && JSON.stringify(item.gitlink) === JSON.stringify(file.gitlink));
        };
        return await openGitlinkDiff({ vscode: this.vscode, links: this.links, repo,
          repoIdentity: result.repo_identity, repoToken: token, file: { ...file, oldPath: file.old_path },
          oldRef: result.base_oid, newRef: result.head_oid, current, verify,
          readHistorical: (commit, relative, oid, mode) => this.backend.submoduleHistoricalContent(repo.rootUri.fsPath,
            result.repo_identity, commit, relative, oid, mode),
          contextKey: worktree?.contextKey,
          title: `${path.basename(file.path)} (${result.base_ref}...HEAD)` });
      }
      const uri = this.vscode.Uri.file(path.join(repo.rootUri.fsPath, file.path));
      const empty = uri.with({ scheme: 'gitpane-empty', query: '' });
      const persistent = () => this.tree.isCurrentRepository(repo, token) && this.tree.git.repositories.some(item =>
        item.rootUri.toString() === repo.rootUri.toString()) && (!worktree || worktree.current());
      const makeSide = (relative, ref) => createLiveGitSide({ vscode: this.vscode, links: this.links, backend: this.backend,
        repo, repoIdentity: result.repo_identity, repoToken: token, side: { gitPath: relative, ref }, current: persistent,
        revision: () => this.tree.contentRevision?.(repo), contextKey: worktree?.contextKey });
      const left = file.status === 'A' ? empty : await makeSide(file.status === 'R' ? file.old_path : file.path, result.base_oid);
      const right = file.status === 'D' ? empty : await makeSide(file.path, result.head_oid);
      if (!this.current(repo, generation, result.head_oid, currentGuard)) throw new Error('Repository or HEAD changed; refresh and try again.');
      await this.vscode.commands.executeCommand('vscode.diff', left, right, `${path.basename(file.path)} (${result.base_ref}...HEAD)`);
    } catch (error) {
      await this.vscode.window.showErrorMessage(`Gitpane branch review: ${error.message}`);
    }
  }

  dispose() {
    this.disposed = true;
    this.generation++;
  }
}

module.exports = { BranchReviewProvider };
