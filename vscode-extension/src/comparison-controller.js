'use strict';

const path = require('node:path');
const { randomUUID } = require('node:crypto');
const { createLiveGitSide } = require('./git-side-documents');
const { openGitlinkDiff } = require('./gitlink-diff');
const { ComparisonService } = require('./comparison-service');

class ComparisonController {
  constructor({ vscode, model, backend, tree, links, repo, token, current, validate, context, actionGuard = () => () => true }) {
    Object.assign(this, { vscode, model, backend, tree, links, repo, token, current, validate, context, actionGuard });
    this.files = new Map();
    this.generation = 0;
    this.verification = new ComparisonService(backend);
  }

  clearFiles() { this.generation++; this.files.clear(); this.verification.cancel(); }
  suspend() { this.generation++; this.verification.cancel(); }

  async compare() {
    this.clearFiles();
    const generation = this.generation;
    const result = await this.model.compare();
    if (generation !== this.generation || !this.current()) throw new Error('Comparison is no longer available.');
    return result.files.map(file => {
      const id = randomUUID();
      this.files.set(id, file);
      return { id, path: file.path, old_path: file.old_path, status: file.status, is_submodule: file.is_submodule };
    });
  }

  async open(id) {
    const file = this.files.get(id), result = this.model.result;
    if (!file || !result || !this.current()) throw new Error('Comparison is no longer available.');
    const generation = this.generation;
    const active = this.actionGuard();
    const selected = () => active() && this.generation === generation && this.files.get(id) === file &&
      this.model.result === result && this.current();
    if (!await this.validate() || !selected()) throw new Error('Repository is no longer available.');
    const root = this.repo.rootUri.fsPath;
    const uri = this.vscode.Uri.file(path.join(root, ...file.path.split('/')));
    const title = `${uri.fsPath} (${result.base_oid.slice(0, 8)} → ${result.to_oid.slice(0, 8)})`;
    if (file.is_submodule) {
      if ([file.gitlink?.old, file.gitlink?.new].some(side => side && side.kind !== 'gitlink')) {
        throw new Error('A submodule changed type. Inspect this change in Git before opening its diff.');
      }
      const verify = async () => {
        if (!selected() || !await this.validate()) return false;
        const latest = await this.verification.request('compare_revisions', root, {
          from: this.model.from.oid, to: this.model.to.oid, common_ancestor: this.model.mode === 'common_ancestor',
          offset: file.comparisonOffset
        }, selected);
        return selected() && latest.repo_identity === result.repo_identity && latest.base_oid === result.base_oid &&
          latest.to_oid === result.to_oid && Array.isArray(latest.files) && latest.files.some(item =>
            item.path === file.path && item.old_path === file.old_path && item.status === file.status &&
            item.is_submodule === true && JSON.stringify(item.gitlink) === JSON.stringify(file.gitlink));
      };
      return await openGitlinkDiff({ vscode: this.vscode, links: this.links, repo: this.repo,
        repoIdentity: result.repo_identity, repoToken: this.token, file: { ...file, oldPath: file.old_path },
        oldRef: result.base_oid, newRef: result.to_oid, current: this.current, verify, title,
        verifyAncestors: this.context?.verifyAncestors, ancestorKey: this.context?.ancestorKey,
        contextKey: this.context?.contextKey });
    }
    const empty = uri.with({ scheme: 'gitpane-empty', query: '' });
    const side = (relative, ref) => createLiveGitSide({ vscode: this.vscode, links: this.links, backend: this.backend,
      repo: this.repo, repoIdentity: result.repo_identity, repoToken: this.token, side: { gitPath: relative, ref },
      current: this.current, literalObjects: true, revision: () => this.tree.contentRevision?.(this.repo),
      verifyAncestors: this.context?.verifyAncestors, ancestorKey: this.context?.ancestorKey, contextKey: this.context?.contextKey });
    const left = file.status === 'A' ? empty : await side(file.status === 'R' ? file.old_path : file.path, result.base_oid);
    const right = file.status === 'D' ? empty : await side(file.path, result.to_oid);
    if (!selected() || !await this.validate() || !selected()) throw new Error('Comparison is no longer available.');
    await this.vscode.commands.executeCommand('vscode.diff', left, right, title);
  }

  dispose() { this.clearFiles(); this.model.dispose(); this.verification.dispose(); }
}

module.exports = { ComparisonController };
