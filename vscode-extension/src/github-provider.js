'use strict';

const fs = require('node:fs/promises');
const { BridgeClient } = require('./bridge-client');

function text(value) {
  return value.replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f\u202a-\u202e\u2066-\u2069]/g, '');
}

function label(value) {
  return text(value).replace(/[\r\n\t]/g, ' ').replace(/\$\(/g, '＄(').slice(0, 400);
}

function validOwner(value) {
  return typeof value === 'string' && value.length <= 39 &&
    /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/i.test(value);
}

function validRepo(value) {
  return typeof value === 'string' && value.length <= 100 && value !== '.' && value !== '..' &&
    !value.startsWith('-') && /^[a-z0-9_.-]+$/i.test(value);
}

function validNumber(value) { return Number.isSafeInteger(value) && value > 0; }
function nullableText(value) { return value === null || typeof value === 'string'; }

class GithubProvider {
  constructor(tree, backend, options = {}) {
    this.tree = tree;
    this.backend = backend;
    this.vscode = options.vscode || require('vscode');
    this.realpath = options.realpath || fs.realpath;
    this.createClient = options.createClient || (executable => new BridgeClient(executable, { timeoutMs: 60000 }));
    this.generation = 0;
    this.disposed = false;
    this.clients = new Set();
  }

  current(repo, generation, snapshot, repoPath) {
    const current = repo && this.tree.git.repositories.find(item => item.rootUri.toString() === repo.rootUri.toString());
    return !this.disposed && generation === this.generation && !!current && this.tree.isVisible(current) &&
      (!snapshot || this.backend.isCurrentMutation(snapshot, repoPath));
  }

  async request(repo, generation, method, params) {
    const snapshot = await this.backend.mutationSnapshot();
    const repoPath = await this.realpath(repo.rootUri.fsPath);
    if (!this.current(repo, generation, snapshot, repoPath)) throw new Error('Repository or workspace changed; reopen GitHub view.');
    const client = this.createClient(snapshot.executable);
    this.clients.add(client);
    let dispatched = false;
    let timedOut = false;
    try {
      await client.initialize(snapshot.roots);
      if (!this.current(repo, generation, snapshot, repoPath)) throw new Error('Repository or workspace changed; reopen GitHub view.');
      // call() sends synchronously after the final scope check. The service is
      // dedicated to this request, so status polling cannot queue behind gh.
      const pending = client.call(method, { repo: repoPath, ...params });
      dispatched = true;
      const value = await pending;
      if (await this.realpath(repo.rootUri.fsPath) !== repoPath || !this.current(repo, generation, snapshot, repoPath)) {
        throw new Error('Repository or workspace changed; reopen GitHub view.');
      }
      return { value, snapshot, repoPath };
    } catch (error) {
      timedOut = dispatched && error?.code === 'timeout';
      throw error;
    } finally {
      if (timedOut && client.child) {
        // Let the service reap its own bounded gh subprocess before exiting.
        const child = client.child;
        child.once('close', () => { client.dispose(); this.clients.delete(client); });
        child.stdin.end();
      } else {
        client.dispose();
        this.clients.delete(client);
      }
    }
  }

  validateList(value) {
    if (!value || !validOwner(value.owner) || !validRepo(value.repo) || value.limit !== 50 ||
      !Array.isArray(value.issues) || !Array.isArray(value.prs) || value.issues.length > 50 || value.prs.length > 50 ||
      !nullableText(value.issues_error) || !nullableText(value.prs_error)) throw new Error('Gitpane returned an invalid GitHub list.');
    const choices = [];
    for (const [kind, items] of [['issue', value.issues], ['pr', value.prs]]) {
      for (const item of items) {
        if (!item || !validNumber(item.number) || typeof item.title !== 'string' || typeof item.state !== 'string' ||
          typeof item.author !== 'string' || typeof item.updated_at !== 'string' || typeof item.draft !== 'boolean' ||
          ![null, 'success', 'failure', 'pending'].includes(item.checks)) throw new Error('Gitpane returned an invalid GitHub item.');
        const details = [item.state, item.author && `by ${item.author}`, item.draft && 'draft', item.checks && `checks: ${item.checks}`].filter(Boolean);
        choices.push({ action: 'item', kind, number: item.number,
          label: `${kind === 'pr' ? 'PR' : 'Issue'} #${item.number} ${label(item.title)}`,
          description: label(details.join(' · ')) });
      }
    }
    for (const [name, error] of [['Issues', value.issues_error], ['Pull requests', value.prs_error]]) {
      if (error) choices.push({ action: 'info', label: `${name} unavailable`, description: label(error) });
    }
    if (!choices.length) choices.push({ action: 'info', label: 'No issues or pull requests' });
    return choices;
  }

  validateDetail(value, list, item) {
    if (!value || value.owner !== list.owner || value.repo !== list.repo || value.kind !== item.kind || value.number !== item.number ||
      typeof value.title !== 'string' || typeof value.author !== 'string' || typeof value.body !== 'string' ||
      !Array.isArray(value.comments) || !nullableText(value.diff) || !nullableText(value.diff_error) ||
      value.comments.some(comment => !comment || typeof comment.author !== 'string' || typeof comment.body !== 'string' || typeof comment.created_at !== 'string')) {
      throw new Error('Gitpane returned an invalid GitHub detail.');
    }
  }

  async detail(repo, generation, list, item) {
    const result = await this.request(repo, generation, 'github_detail', { kind: item.kind, number: item.number });
    this.validateDetail(result.value, list, item);
    const detail = result.value;
    const lines = [`${item.kind === 'pr' ? 'Pull request' : 'Issue'} #${item.number}: ${text(detail.title)}`,
      `Author: ${text(detail.author)}`, '', text(detail.body), '', 'Comments'];
    for (const comment of detail.comments) lines.push('', `${text(comment.author)} · ${text(comment.created_at)}`, text(comment.body));
    if (!detail.comments.length) lines.push('No comments.');
    if (detail.diff_error) lines.push('', `Pull request diff unavailable: ${text(detail.diff_error)}`);
    else if (detail.diff !== null) lines.push('', 'Pull request diff', text(detail.diff));
    if (!this.current(repo, generation, result.snapshot, result.repoPath)) throw new Error('Repository or workspace changed; reopen GitHub view.');
    const document = await this.vscode.workspace.openTextDocument({ language: 'plaintext', content: lines.join('\n') });
    if (!this.current(repo, generation, result.snapshot, result.repoPath)) return;
    await this.vscode.window.showTextDocument(document, { preview: false });
    if (!this.current(repo, generation, result.snapshot, result.repoPath)) return;
    const choices = [{ label: 'Open on GitHub', action: 'browser' }, { label: 'Back to GitHub list', action: 'back' }];
    const selected = await this.vscode.window.showQuickPick(choices, { placeHolder: 'GitHub item actions' });
    if (!selected || !choices.includes(selected) || selected.action !== 'browser') return;
    if (await this.realpath(repo.rootUri.fsPath) !== result.repoPath || !this.current(repo, generation, result.snapshot, result.repoPath)) {
      throw new Error('Repository or workspace changed; reopen GitHub view.');
    }
    const type = item.kind === 'pr' ? 'pull' : 'issues';
    const url = `https://github.com/${list.owner}/${list.repo}/${type}/${item.number}`;
    await this.vscode.env.openExternal(this.vscode.Uri.parse(url));
  }

  async open(repo) {
    const generation = ++this.generation;
    if (!this.current(repo, generation)) return;
    let state = 'open';
    let data;
    try {
      while (this.current(repo, generation)) {
        if (!data) data = await this.request(repo, generation, 'github_list', { state });
        if (!this.current(repo, generation, data.snapshot, data.repoPath)) return;
        const choices = [
          { label: 'Refresh', action: 'refresh' },
          { label: `State: ${state}`, action: 'filter' },
          ...this.validateList(data.value)
        ];
        const selected = await this.vscode.window.showQuickPick(choices, {
          placeHolder: `${data.value.owner}/${data.value.repo} · GitHub`, matchOnDescription: true
        });
        if (!selected || !choices.includes(selected) || !this.current(repo, generation, data.snapshot, data.repoPath)) return;
        if (selected.action === 'refresh') data = undefined;
        else if (selected.action === 'filter') {
          const filters = ['open', 'all', 'closed'].map(value => ({ label: value, value }));
          const filter = await this.vscode.window.showQuickPick(filters, { placeHolder: 'GitHub item state' });
          if (!filter || !filters.includes(filter) || !this.current(repo, generation, data.snapshot, data.repoPath)) return;
          state = filter.value;
          data = undefined;
        } else if (selected.action === 'item') {
          await this.detail(repo, generation, data.value, selected);
        }
      }
    } catch (error) {
      if (this.current(repo, generation)) await this.vscode.window.showErrorMessage(`Gitpane GitHub: ${error.message}`);
    }
  }

  dispose() {
    this.disposed = true;
    this.generation++;
  }
}

module.exports = { GithubProvider };
