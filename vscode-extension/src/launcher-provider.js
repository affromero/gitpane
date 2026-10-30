'use strict';

const path = require('node:path');

function text(value) {
  return value.replace(/[\x00-\x1f\x7f\u202a-\u202e\u2066-\u2069]/g, ' ')
    .replace(/\$\(/g, '＄(').trim().slice(0, 200) || 'Launcher';
}

function validString(value) {
  return typeof value === 'string' && value.length > 0 && !value.includes('\0');
}

function identity(value) {
  return typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
}

function supportedExecutable(value) {
  const name = path.win32.basename(path.posix.basename(value)).toLowerCase();
  if (/\.(?:bat|cmd|ps1|sh)$/.test(name)) return false;
  const base = name.replace(/\.exe$/, '');
  return !/^(?:sh|bash|zsh|dash|ksh|fish|csh|tcsh|ash|cmd|powershell|pwsh|osascript|env|perl|ruby|node|deno|bun|php|lua|awk|sed|xargs|python(?:\d+(?:\.\d+)*)?)$/.test(base);
}

function terminalOptions(plan, repo, title, fingerprint) {
  if (!plan || plan.fingerprint !== fingerprint) throw new Error('Launcher configuration changed. Choose it again.');
  const options = { name: `Gitpane: ${text(title)}`, cwd: repo.rootUri };
  if (plan.kind === 'terminal') return options;
  if (plan.kind === 'argv') {
    if (!Array.isArray(plan.argv) || !plan.argv.length || !validString(plan.argv[0]) ||
        !plan.argv.every(arg => typeof arg === 'string' && !arg.includes('\0')) ||
        !supportedExecutable(plan.argv[0])) {
      throw new Error('Unsupported or invalid launcher command.');
    }
    options.shellPath = plan.argv[0];
    options.shellArgs = plan.argv.slice(1);
    return options;
  }
  if (plan.kind === 'shell') {
    const env = plan.env;
    if (process.platform === 'win32' || !validString(plan.script) || !env || typeof env !== 'object' ||
        Array.isArray(env) || Object.keys(env).sort().join(',') !== 'GITPANE_LAUNCH_BASE,GITPANE_LAUNCH_PATH' ||
        typeof env.GITPANE_LAUNCH_BASE !== 'string' || env.GITPANE_LAUNCH_BASE.includes('\0') ||
        env.GITPANE_LAUNCH_BASE !== (plan.base_ref ?? '') || !validString(env.GITPANE_LAUNCH_PATH)) {
      throw new Error('Unsupported or invalid shell launcher plan.');
    }
    options.shellPath = 'sh';
    options.shellArgs = ['-c', plan.script];
    options.env = { GITPANE_LAUNCH_PATH: env.GITPANE_LAUNCH_PATH, GITPANE_LAUNCH_BASE: env.GITPANE_LAUNCH_BASE };
    return options;
  }
  throw new Error('Unsupported launcher placement.');
}

class LauncherProvider {
  constructor(tree, backend, options = {}) {
    this.tree = tree;
    this.backend = backend;
    this.vscode = options.vscode || require('vscode');
    this.generation = 0;
    this.disposed = false;
  }

  current(repo, generation, guard = () => true) {
    if (!guard() || !repo?.rootUri || this.disposed || generation !== this.generation) return null;
    const current = this.tree.git.repositories.find(item => item.rootUri.toString() === repo.rootUri.toString());
    return current && this.tree.isVisible(current) ? current : null;
  }

  requireCurrent(repo, generation, guard) {
    const current = this.current(repo, generation, guard);
    if (!current) throw new Error('Repository or workspace changed; choose the launcher again.');
    return current;
  }

  async openMultiplexer(repo, generation, guard, action, plan) {
    if (plan.fingerprint !== action.fingerprint || !identity(plan.repo_identity)) {
      throw new Error('Launcher configuration or repository changed. Choose it again.');
    }
    const configured = this.vscode.workspace?.getConfiguration('gitpane').get('sessionBackend', 'auto') ?? 'auto';
    if (!['auto', 'tmux', 'herdr'].includes(configured)) throw new Error('Choose auto, tmux, or herdr for gitpane.sessionBackend.');
    let backend = configured;
    if (backend === 'auto') {
      const backends = [{ label: 'tmux', kind: 'tmux' }, { label: 'herdr', kind: 'herdr' }];
      const selected = await this.vscode.window.showQuickPick(backends, { placeHolder: 'Choose a session type' });
      this.requireCurrent(repo, generation, guard);
      if (!selected || !backends.includes(selected)) return;
      backend = selected.kind;
    }
    const baseArgs = action.kind === 'review' ? [plan.base_ref] : [];
    const result = await this.backend.launcherChoices(repo.rootUri.fsPath, action.id, action.fingerprint, backend, ...baseArgs);
    this.requireCurrent(repo, generation, guard);
    if (!result || result.repo_identity !== plan.repo_identity || !Array.isArray(result.choices) ||
        (result.notice != null && !validString(result.notice))) {
      throw new Error('Gitpane returned invalid launcher placement choices.');
    }
    const ids = new Set();
    const choices = result.choices.map(choice => {
      if (!choice || !validString(choice.id) || !validString(choice.label) || !identity(choice.context_token) || ids.has(choice.id)) {
        throw new Error('Gitpane returned invalid launcher placement choices.');
      }
      ids.add(choice.id);
      return { label: text(choice.label), id: choice.id, contextToken: choice.context_token };
    });
    let target;
    if (plan.kind === 'multiplexer') {
      if (choices.length !== 1 || choices[0].id !== 'configured') throw new Error('Gitpane returned invalid configured launcher placement.');
      target = choices[0];
    } else {
      if (!choices.length) {
        await this.vscode.window.showInformationMessage(result.notice ? text(result.notice) : `No ${backend} launcher placements are available.`);
        return;
      }
      target = await this.vscode.window.showQuickPick(choices, { placeHolder: result.notice ? `Choose where to open: ${text(result.notice)}` : 'Choose where to open' });
      this.requireCurrent(repo, generation, guard);
      if (!target || !choices.includes(target)) return;
    }
    const validation = await this.backend.validateLauncher(repo.rootUri.fsPath, action.id, action.fingerprint, ...baseArgs);
    this.requireCurrent(repo, generation, guard);
    if (!validation || validation.valid !== true || (action.kind === 'review' && validation.base_ref !== plan.base_ref)) throw new Error('Launcher configuration changed. Choose it again.');
    await this.backend.executeLauncher(repo.rootUri.fsPath, action.id, action.fingerprint,
      plan.repo_identity, backend, target.id, target.contextToken, () => !!this.current(repo, generation, guard), ...baseArgs);
  }

  async open(repo, guard = () => true, selector) {
    const generation = ++this.generation;
    const token = repo && this.tree.repositoryToken(repo);
    const currentGuard = () => guard() && this.tree.isCurrentRepository(repo, token);
    let terminal;
    try {
      this.requireCurrent(repo, generation, currentGuard);
      const result = await this.backend.listLaunchers(repo.rootUri.fsPath);
      this.requireCurrent(repo, generation, currentGuard);
      if (!result || !Array.isArray(result.actions)) throw new Error('Gitpane returned an invalid launcher list.');
      const ids = new Set();
      const choices = result.actions.map(action => {
        if (!action || !validString(action.id) || !validString(action.label) || !['open', 'review', 'keybinding'].includes(action.kind) ||
            !validString(action.fingerprint) || ids.has(action.id)) throw new Error('Gitpane returned an invalid launcher list.');
        ids.add(action.id);
        return { label: text(action.label), id: action.id, kind: action.kind, key: action.key, fingerprint: action.fingerprint };
      });
      if (!choices.length) {
        await this.vscode.window.showInformationMessage('No configured Gitpane launchers for this repository.');
        return;
      }
      const selected = selector
        ? choices.find(action => selector.id ? action.id === selector.id : action.kind === 'keybinding' && action.key === selector.key)
        : await this.vscode.window.showQuickPick(choices, { placeHolder: 'Choose a Gitpane launcher' });
      if (selector && !selected) throw new Error('The configured launcher changed. Refresh and try again.');
      this.requireCurrent(repo, generation, currentGuard);
      if (!selected || !choices.includes(selected)) return;
      const plan = await this.backend.planLauncher(repo.rootUri.fsPath, selected.id, selected.fingerprint);
      let current = this.requireCurrent(repo, generation, currentGuard);
      if (selected.kind === 'review' && !(plan?.base_ref === null ||
          (typeof plan?.base_ref === 'string' && !plan.base_ref.includes('\0')))) throw new Error('Gitpane returned an invalid review base.');
      if (plan?.kind === 'multiplexer' || plan?.kind === 'ask') {
        await this.openMultiplexer(repo, generation, currentGuard, selected, plan);
        return;
      }
      const options = terminalOptions(plan, current, selected.label, selected.fingerprint);
      const validation = await this.backend.validateLauncher(repo.rootUri.fsPath, selected.id, selected.fingerprint,
        ...(selected.kind === 'review' ? [plan.base_ref] : []));
      current = this.requireCurrent(repo, generation, currentGuard);
      if (!validation || validation.valid !== true || (selected.kind === 'review' && validation.base_ref !== plan.base_ref)) throw new Error('Launcher configuration changed. Choose it again.');
      options.cwd = current.rootUri;
      terminal = this.vscode.window.createTerminal(options);
      terminal.show();
    } catch (error) {
      terminal?.dispose();
      await this.vscode.window.showErrorMessage(`Gitpane launcher: ${error?.message || String(error)}`);
    }
  }

  dispose() {
    this.disposed = true;
    this.generation++;
  }
}

module.exports = { LauncherProvider };
