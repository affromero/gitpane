'use strict';

const path = require('node:path');

class WorkspaceFetch {
  constructor(options) {
    this.tree = options.tree;
    this.backend = options.backend;
    this.mutations = options.mutations;
    this.vscode = options.vscode;
    this.output = options.output;
    this.refresh = options.refresh;
    this.active = false;
  }

  current(target) {
    return this.backend.generation === target.scopeGeneration && this.tree.fetchTargets().some(item => item.path === target.path && item.guard === target.guard &&
      item.commonIdentity === target.commonIdentity && item.commonDir === target.commonDir && item.repoIdentity === target.repoIdentity);
  }

  async run() {
    if (this.active) {
      await this.vscode.window.showInformationMessage('Gitpane is already fetching the workspace.');
      return;
    }
    this.active = true;
    try {
      const targets = this.tree.fetchTargets().map(target => ({ ...target, scopeGeneration: this.backend.generation }));
      if (!targets.length) {
        await this.vscode.window.showInformationMessage('No repositories found in the workspace.');
        return;
      }
      const result = await this.vscode.window.withProgress({
        location: this.vscode.ProgressLocation.Notification, title: 'Gitpane: Fetch Workspace', cancellable: false
      }, progress => this.fetch(targets, progress));
      let refreshFailure;
      try { await this.refresh(); }
      catch (error) { refreshFailure = error?.message || String(error); }
      if (!result.failures.length && !refreshFailure) {
        await this.vscode.window.showInformationMessage(`Fetched ${result.fetched} ${result.fetched === 1 ? 'repository' : 'repositories'}.`);
        return;
      }
      for (const failure of result.failures) this.output.appendLine(`Gitpane fetch failed for ${failure.path}: ${failure.message}`);
      if (refreshFailure) this.output.appendLine(`Gitpane could not refresh after fetch: ${refreshFailure}`);
      const sample = result.failures.slice(0, 3).map(failure => `${failure.path}: ${failure.message}`).join('\n');
      const more = result.failures.length > 3 ? `\n${result.failures.length - 3} more in Gitpane output.` : '';
      const refreshMessage = refreshFailure ? `\nCould not refresh after fetch: ${refreshFailure}` : '';
      const selected = await this.vscode.window.showWarningMessage(
        `Fetched ${result.fetched} of ${targets.length} repositories. ${result.failures.length} failed.${sample ? `\n${sample}` : ''}${more}${refreshMessage}`, 'Show Details');
      if (selected === 'Show Details') this.output.show();
    } finally { this.active = false; }
  }

  async fetch(targets, progress) {
    const failures = [];
    let fetched = 0;
    let next = 0;
    const prepared = new Array(targets.length);
    const recordFailure = (target, error) => {
      const prefix = error?.code === 'outcome_unknown' ? 'Fetch outcome is unknown. Inspect the repository before retrying: ' : '';
      failures.push({ path: target.path, message: prefix + (error?.message || String(error)) });
      progress.report({ increment: 100 / targets.length, message: target.path });
    };
    const prepare = async () => {
      while (next < targets.length) {
        const index = next++;
        const target = targets[index];
        try {
          if (!this.current(target)) throw new Error('Repository or workspace changed before fetch.');
          if (!target.commonIdentity || typeof target.commonDir !== 'string' || !path.isAbsolute(target.commonDir)) {
            throw new Error('Git common directory is unavailable. Refresh and try again.');
          }
          if (typeof target.repoIdentity !== 'string' || !/^[a-f0-9]{64}$/.test(target.repoIdentity)) {
            throw new Error('Repository identity is unavailable. Update the backend and refresh before fetching.');
          }
          const preview = await this.backend.previewFetch(target.path);
          if (!preview || typeof preview.repo_identity !== 'string' || !/^[a-f0-9]{64}$/.test(preview.repo_identity) ||
            typeof preview.fetch_token !== 'string' || !/^[a-f0-9]{64}$/.test(preview.fetch_token)) {
            throw new Error('Gitpane returned an invalid fetch preview. Update the backend and try again.');
          }
          if (preview.repo_identity !== target.repoIdentity || !this.current(target)) {
            throw new Error('Repository or workspace changed before fetch.');
          }
          prepared[index] = { target, preview };
        } catch (error) { recordFailure(target, error); }
      }
    };
    await Promise.all(Array.from({ length: Math.min(3, targets.length) }, prepare));
    const groups = new Map();
    for (const entry of prepared) {
      if (!entry) continue;
      const key = entry.target.commonIdentity;
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(entry);
    }
    const pending = [...groups.values()];
    next = 0;
    const worker = async () => {
      while (next < pending.length) {
        for (const { target, preview } of pending[next++]) {
          try {
            if (!this.current(target)) throw new Error('Repository or workspace changed before fetch.');
            await this.mutations.execute('repo_fetch', { repo: target.path, expected_repo_identity: preview.repo_identity,
              expected_fetch_token: preview.fetch_token }, {
              lockKey: target.commonDir, validateTarget: () => this.current(target), waitForClose: true
            });
            fetched++;
          } catch (error) {
            recordFailure(target, error);
            continue;
          }
          progress.report({ increment: 100 / targets.length, message: target.path });
        }
      }
    };
    await Promise.all(Array.from({ length: Math.min(3, pending.length) }, worker));
    return { fetched, failures };
  }
}

module.exports = { WorkspaceFetch };
