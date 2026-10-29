'use strict';

const path = require('node:path');

const FILE_ACTIONS = new Set(['open', 'openFile', 'reveal', 'copy', 'stage', 'unstage', 'discard']);
const REPO_ACTIONS = new Set(['createWorktree', 'reviewBranch', 'github', 'session', 'openTerminal', 'openLauncher', 'syncSubmodules', 'updateSubmodules', 'pullSubmodules', 'pullWithSubmodules', 'revealRepo', 'copyRepo', 'push', 'pull', 'pull_rebase']);
const WORKTREE_ACTIONS = new Set(['removeWorktree', 'openWorktreeGraph', 'reviewWorktree', 'worktreeRepositoryActions']);

function fingerprint(item, kind) {
  if (!item || typeof item.repoPath !== 'string' || !path.isAbsolute(item.repoPath)) return undefined;
  const fields = [item.repoPath, item.commonDir];
  if (kind === 'repo') fields.push(item.branch, item.head);
  if (kind === 'file') fields.push(item.path, item.group, item.status, !!item.isSubmodule);
  if (kind === 'worktree') fields.push(item.path, item.branch, !!item.available, item.childRepoKey, item.childRepoId);
  return JSON.stringify(fields);
}

function validFile(item) {
  if (!item || typeof item.path !== 'string' || !item.path || path.isAbsolute(item.path)) return false;
  const parts = item.path.split(/[\\/]/);
  return !parts.some(part => part === '..' || part === '.' || part === '');
}

function fileOptions(item) {
  if (!validFile(item)) return [];
  const choices = [
    { label: 'Open Changes', action: 'open' },
    { label: 'Open File', action: 'openFile' },
    { label: 'Reveal in Explorer', action: 'reveal' },
    { label: 'Copy Path', action: 'copy' }
  ];
  if (item.isSubmodule) return choices;
  if (item.group === 'index') choices.push({ label: 'Unstage', action: 'unstage' });
  if (['working', 'untracked', 'merge'].includes(item.group)) choices.push({ label: 'Stage', action: 'stage' });
  if (['index', 'working', 'untracked'].includes(item.group)) choices.push({
    label: item.group === 'untracked' ? 'Delete Untracked File' : 'Discard Changes', action: 'discard'
  });
  return choices;
}

class SidebarActions {
  constructor(options) {
    this.vscode = options.vscode;
    this.mutations = options.mutations;
    this.resolveRepository = options.resolveRepository;
    this.resolveChange = options.resolveChange;
    this.resolveWorktree = options.resolveWorktree;
    this.refresh = options.refresh;
    this.onWorktreeCreated = options.onWorktreeCreated;
    this.reviewBranch = options.reviewBranch;
    this.openGithub = options.openGithub;
    this.openSession = options.openSession;
    this.openTerminal = options.openTerminal;
    this.openLauncher = options.openLauncher;
    this.openWorktreeGraph = options.openWorktreeGraph;
    this.reviewWorktree = options.reviewWorktree;
    this.previewFileAction = options.previewFileAction;
  }

  resolve(kind, id) {
    if (typeof id !== 'string') return undefined;
    if (kind === 'repo') return this.resolveRepository(id);
    if (kind === 'file') return this.resolveChange(id);
    if (kind === 'worktree') return this.resolveWorktree(id);
    return undefined;
  }

  current(kind, id, expected) {
    return !!expected && fingerprint(this.resolve(kind, id), kind) === expected;
  }

  async showMenu(kind, id, guard = () => true) {
    try {
      if (!guard()) return;
      const item = this.resolve(kind, id);
      const expected = fingerprint(item, kind);
      if (!expected) return;
      let choices;
      if (kind === 'file') choices = fileOptions(item);
      else if (kind === 'repo') choices = [
        { label: 'Create Worktree', action: 'createWorktree' },
        { label: 'Review Branch', action: 'reviewBranch' },
        { label: 'GitHub Issues and Pull Requests', action: 'github' },
        { label: 'Sessions', action: 'session' },
        { label: 'Open Terminal', action: 'openTerminal' },
        { label: 'Configured Launchers', action: 'openLauncher' },
        { label: 'Sync Direct Submodules', action: 'syncSubmodules' },
        { label: 'Update Submodules Recursively', action: 'updateSubmodules' },
        { label: 'Pull Direct Submodules', action: 'pullSubmodules' },
        { label: 'Pull with Submodules', action: 'pullWithSubmodules' },
        { label: 'Reveal in Explorer', action: 'revealRepo' },
        { label: 'Copy Path', action: 'copyRepo' },
        { label: 'Push', action: 'push' },
        { label: 'Pull', action: 'pull' },
        { label: 'Pull with Rebase', action: 'pull_rebase' }
      ];
      else if (kind === 'worktree') choices = [
        ...(item.available ? [{ label: 'Open Commit Graph', action: 'openWorktreeGraph' },
          { label: 'Review Branch', action: 'reviewWorktree' },
          { label: 'Repository Actions', action: 'worktreeRepositoryActions' }] : []),
        { label: 'Remove Worktree', action: 'removeWorktree' }
      ];
      else return;
      if (!choices.length) return;
      const selected = await this.vscode.window.showQuickPick(choices, { placeHolder: 'Choose a Gitpane action' });
      if (!selected || !choices.includes(selected) || !guard() || !this.current(kind, id, expected)) return;
      await this.handle({ type: 'action', action: selected.action, id }, expected, guard);
    } catch (error) {
      await this.report(error);
    }
  }

  async report(error) {
    const message = error?.message || String(error);
    const prefix = error?.code === 'outcome_unknown' ? 'Gitpane action outcome is unknown. Refresh and inspect the repository: ' : 'Gitpane action failed: ';
    await this.vscode.window.showErrorMessage(prefix + message);
  }

  async handle(message, expected, guard = () => true) {
    const action = message?.action;
    const kind = FILE_ACTIONS.has(action) ? 'file' : REPO_ACTIONS.has(action) ? 'repo' : WORKTREE_ACTIONS.has(action) ? 'worktree' : undefined;
    if (message?.type !== 'action' || !kind || typeof message.id !== 'string') return;
    try {
      const item = this.resolve(kind, message.id);
      const original = fingerprint(item, kind);
      if (!original || (expected && original !== expected) || !guard()) return;
      const current = () => guard() && this.current(kind, message.id, original);
      if (kind === 'file' && !fileOptions(item).some(choice => choice.action === action)) return;
      if (kind === 'worktree' && action !== 'removeWorktree' && !item.available) return;
      if (action === 'worktreeRepositoryActions') {
        if (typeof item.childRepoId === 'string') await this.showMenu('repo', item.childRepoId, current);
        return;
      }
      if (action === 'openWorktreeGraph') {
        if (this.openWorktreeGraph) await this.openWorktreeGraph(message.id, current);
        return;
      }
      if (action === 'reviewWorktree') {
        if (this.reviewWorktree) await this.reviewWorktree(message.id, current);
        return;
      }
      if (action === 'open') {
        await this.vscode.commands.executeCommand('gitpane.openChange', item.node);
        return;
      }
      if (action === 'openFile') {
        try { await this.vscode.workspace.fs.stat(item.uri); }
        catch (error) { throw new Error(`File is unavailable: ${error.message}`); }
        if (!current()) return;
        await this.vscode.commands.executeCommand('vscode.open', item.uri);
        return;
      }
      if (action === 'reveal') {
        await this.vscode.commands.executeCommand('revealInExplorer', item.uri);
        return;
      }
      if (action === 'copy') {
        await this.vscode.env.clipboard.writeText(path.join(item.repoPath, item.path));
        return;
      }
      if (action === 'reviewBranch') {
        if (this.reviewBranch) await this.reviewBranch(message.id, current);
        return;
      }
      if (action === 'github') {
        if (this.openGithub) await this.openGithub(message.id, current);
        return;
      }
      if (action === 'session') {
        if (this.openSession) await this.openSession(message.id, current);
        return;
      }
      if (action === 'openTerminal') {
        if (this.openTerminal) await this.openTerminal(message.id, current);
        return;
      }
      if (action === 'openLauncher') {
        if (this.openLauncher) await this.openLauncher(message.id, current);
        return;
      }
      if (action === 'revealRepo') {
        await this.vscode.commands.executeCommand('revealInExplorer', this.vscode.Uri.file(item.repoPath));
        return;
      }
      if (action === 'copyRepo') {
        await this.vscode.env.clipboard.writeText(item.repoPath);
        return;
      }
      let method;
      let params;
      if (kind === 'file') {
        let operation = action;
        let token;
        if (action === 'discard') {
          operation = item.group === 'untracked' ? 'delete_untracked' : 'discard';
          if (typeof this.previewFileAction !== 'function') throw new Error('File confirmation is unavailable.');
          const preview = await this.previewFileAction(item.repoPath, item.path, operation);
          if (!preview || typeof preview.token !== 'string' || !/^[a-f0-9]{64}$/.test(preview.token)) {
            throw new Error('Gitpane returned an invalid file confirmation.');
          }
          if (!current()) return;
          token = preview.token;
          const detail = operation === 'delete_untracked'
            ? `Delete untracked file ${item.path}? This cannot be undone.`
            : `Discard staged and working changes for ${item.path}? A staged new file may be deleted. This cannot be undone.`;
          if (await this.vscode.window.showWarningMessage(detail, { modal: true }, 'Discard') !== 'Discard') return;
        }
        method = 'file_action';
        params = { repo: item.repoPath, path: item.path, operation,
          ...(token ? { expected_token: token } : {}) };
      } else if (kind === 'repo' && action !== 'createWorktree') {
        const submodules = ['syncSubmodules', 'updateSubmodules', 'pullSubmodules', 'pullWithSubmodules'].includes(action);
        method = submodules ? 'submodule_action' : 'repo_sync';
        if (typeof item.head !== 'string' || !/^[a-f0-9]{40,64}$/i.test(item.head)) {
          throw new Error('Cannot sync a repository without a current HEAD commit.');
        }
        const submoduleOperations = { syncSubmodules: 'sync', updateSubmodules: 'update', pullSubmodules: 'pull_latest', pullWithSubmodules: 'pull_with_submodules' };
        params = { repo: item.repoPath, operation: submodules ? submoduleOperations[action] : action,
          expected_branch: item.branch ?? null, expected_head: item.head };
      } else if (action === 'createWorktree') {
        const branch = await this.vscode.window.showInputBox({ prompt: 'New worktree branch name' });
        if (!branch || !current()) return;
        const target = await this.vscode.window.showInputBox({
          prompt: 'Absolute path for the new worktree inside a workspace folder',
          validateInput: value => path.isAbsolute(value) ? undefined : 'Enter an absolute path.'
        });
        if (!target) return;
        if (!path.isAbsolute(target)) throw new Error('Worktree path must be absolute.');
        method = 'worktree_create';
        params = { repo: item.repoPath, branch, path: target };
      } else {
        if (typeof item.path !== 'string' || !path.isAbsolute(item.path)) return;
        if (await this.vscode.window.showWarningMessage(`Remove worktree ${item.path}?`, { modal: true }, 'Remove') !== 'Remove') return;
        method = 'worktree_remove';
        params = { repo: item.repoPath, path: item.path };
      }
      if (!current()) return;
      let actionError;
      try {
        const result = await this.mutations.execute(method, params, {
          lockKey: item.commonDir,
          validateTarget: current
        });
        if (method === 'submodule_action') {
          if (!result || !Array.isArray(result.completed) || !Array.isArray(result.skipped) ||
            result.completed.length > 128 || result.skipped.length > 128 ||
            ![...result.completed, ...result.skipped].every(value => typeof value === 'string')) {
            throw new Error('Gitpane returned an invalid submodule result.');
          }
          const verb = params.operation === 'sync' ? 'Synced' : params.operation === 'pull_latest' ? 'Pulled' : 'Updated';
          const scope = ['update', 'pull_with_submodules'].includes(params.operation) ? '' : ' direct';
          if (params.operation === 'pull_with_submodules' && result.parent_pulled !== true) throw new Error('Gitpane returned an invalid parent pull result.');
          const message = `${params.operation === 'pull_with_submodules' ? 'Pulled repository. ' : ''}${verb} ${result.completed.length}${scope} submodule(s).${result.skipped.length ? ` Skipped ${result.skipped.join(', ')}.` : ''}`;
          await this.vscode.window.showInformationMessage(message);
        }
        if (method === 'worktree_create' && this.onWorktreeCreated) {
          await this.onWorktreeCreated(params.path);
        }
      } catch (error) {
        actionError = error;
      }
      try {
        await this.refresh();
      } catch (error) {
        if (!actionError) throw error;
      }
      if (actionError) throw actionError;
    } catch (error) {
      await this.report(error);
    }
  }
}

module.exports = { SidebarActions };
