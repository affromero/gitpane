'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { relativeFile } = require('./change-pager');

const STATE_KEY = 'gitpane.submodulePins';
const MAX_PINS = 128;
const token = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
const canonical = value => {
  try { return fs.realpathSync.native(value); } catch { return undefined; }
};

function validPin(pin) {
  return !!pin && Object.keys(pin).sort().join(',') === 'chain,root' && typeof pin.root === 'string' && path.isAbsolute(pin.root) && !pin.root.includes('\0') && pin.root.length <= 4096 &&
    Array.isArray(pin.chain) && pin.chain.length > 0 && pin.chain.length <= 32 && pin.chain.every(link =>
      link && Object.keys(link).sort().join(',') === 'child_identity,parent_identity,path' &&
      relativeFile(link.path) && link.path.length <= 4096 && token(link.parent_identity) && token(link.child_identity));
}

function readPins(value) {
  if (value === undefined) return [];
  if (!value || value.version !== 1 || !Array.isArray(value.pins) || value.pins.length > MAX_PINS ||
      !value.pins.every(validPin) || Buffer.byteLength(JSON.stringify(value)) > 1024 * 1024) {
    throw new Error('Saved submodule pins are invalid. Remove the Gitpane submodule pin workspace state and try again.');
  }
  const keys = value.pins.map(pin => JSON.stringify(pin));
  if (new Set(keys).size !== keys.length) throw new Error('Saved submodule pins contain duplicates.');
  return structuredClone(value.pins);
}

class SubmodulePins {
  constructor(tree, backend, git, state, vscode, options = {}) {
    this.tree = tree;
    this.backend = backend;
    this.git = git;
    this.state = state;
    this.vscode = vscode;
    this.revalidateIntervalMs = options.revalidateIntervalMs ?? 5000;
    this.restoreDelayMs = options.restoreDelayMs ?? 50;
    this.openTimeoutMs = options.openTimeoutMs ?? 10000;
    this.pins = [];
    this.active = new Map();
    this.openings = new Map();
    this.version = 0;
    this.pending = Promise.resolve();
    this.disposed = false;
    this.visible = false;
    this.visibilityEpoch = 0;
    this.restoreQueued = false;
    this.restoreAgain = false;
    this.restoreEpoch = 0;
    this.interactive = 0;
    this.error = undefined;
    this.timer = undefined;
    this.signature = undefined;
    try { this.rawState = state?.get(STATE_KEY); this.pins = readPins(this.rawState); }
    catch (error) { this.error = error.message; this.storageError = error.message; }
    this.subscription = tree.onDidChangeTreeData?.(() => this.scheduleRestore());
    this.scheduleRestore();
  }

  key(pin) { return JSON.stringify(pin); }

  isPinned(repo) {
    const root = canonical(repo?.rootUri?.fsPath);
    return !!root && [...this.active].some(([key, item]) => item.path === root &&
      this.pins.some(pin => this.key(pin) === key && this.root(pin)) && this.tree.isCurrentRepository(repo, item.token));
  }

  root(pin) {
    const root = canonical(pin.root);
    if (!root) return undefined;
    const repo = this.git.repositories.find(candidate => canonical(candidate.rootUri.fsPath) === root);
    const currentToken = repo && this.tree.repositoryToken(repo);
    return repo && this.tree.isCurrentRepository(repo, currentToken) ? { repo, token: currentToken, path: root } : undefined;
  }

  current(root, generation) {
    return !this.disposed && this.backend.generation === generation &&
      this.tree.isCurrentRepository(root.repo, root.token) && canonical(root.repo.rootUri.fsPath) === root.path;
  }

  async validate(pin, root, generation, session) {
    if (!this.current(root, generation)) throw new Error('The pinned submodule root is no longer in the workspace.');
    const response = session ? await session.validate(pin.root, pin.chain) : await this.backend.validateSubmodulePin(pin.root, pin.chain);
    if (!this.current(root, generation)) throw new Error('The workspace changed while validating the submodule pin.');
    if (!response || !Array.isArray(response.repos) || response.repos.length !== pin.chain.length + 1 ||
      response.repos.some(value => typeof value !== 'string' || !path.isAbsolute(value))) {
      throw new Error('Gitpane returned invalid submodule pin details.');
    }
    const roots = response.repos.map(canonical);
    if (roots.some(value => !value || !this.tree.isPathInWorkspace(value)) || roots[0] !== root.path) {
      throw new Error('The pinned submodule is outside the workspace or unavailable.');
    }
    for (let index = 0; index < pin.chain.length; index++) {
      const child = canonical(path.join(roots[index], ...pin.chain[index].path.split('/')));
      if (!child || child !== roots[index + 1]) throw new Error('The pinned submodule path changed.');
    }
    return roots;
  }

  async open(pin, guard = () => true, session) {
    const root = this.root(pin);
    const generation = this.backend.generation;
    if (!root || !guard()) throw new Error('The pinned submodule root is no longer in the workspace.');
    const before = await this.validate(pin, root, generation, session);
    const currentChain = () => this.current(root, generation) && guard() && before.every((value, index) => {
      const source = index ? path.join(before[index - 1], ...pin.chain[index - 1].path.split('/')) : pin.root;
      return canonical(source) === value && this.tree.isPathInWorkspace(value);
    });
    if (!currentChain()) throw new Error('The selected submodule changed.');
    const target = before.at(-1);
    let repo = this.git.repositories.find(candidate => canonical(candidate.rootUri.fsPath) === target);
    const inFlight = [...this.openings.values()].find(opening => opening.target === target);
    if (!repo && inFlight) {
      throw Object.assign(new Error('VS Code is still opening this submodule. Try again after it finishes.'), { code: 'open_pending' });
    }
    const existingToken = repo && this.tree.repositoryToken(repo);
    const uri = this.vscode.Uri.file(target);
    const revision = this.tree.repositoryRevision(uri);
    if (!repo) {
      const key = this.key(pin);
      const opening = { target };
      opening.promise = Promise.resolve().then(() => {
        if (!currentChain()) throw new Error('The pinned submodule changed before opening.');
        return this.git.openRepository(uri);
      }).then(value => ({ value }), error => ({ error })).finally(() => {
        if (this.openings.get(key) === opening) this.openings.delete(key);
      });
      this.openings.set(key, opening);
      let timer;
      const timeout = new Promise(resolve => {
        timer = setTimeout(() => resolve({ pending: true }), this.openTimeoutMs);
        timer.unref?.();
      });
      const outcome = await Promise.race([opening.promise, timeout]);
      clearTimeout(timer);
      if (outcome.pending) throw Object.assign(new Error('VS Code is still opening this submodule. Try again after it finishes.'), { code: 'open_pending' });
      if (outcome.error) throw outcome.error;
      repo = outcome.value;
    }
    if (!repo || canonical(repo.rootUri.fsPath) !== target || !currentChain()) {
      throw new Error('The pinned submodule could not be opened in the workspace.');
    }
    const found = this.git.repositories.find(candidate => candidate.rootUri.toString() === repo.rootUri.toString());
    const repoToken = found && this.tree.repositoryToken(found);
    if (!found || !this.tree.isCurrentRepository(found, repoToken) ||
      (existingToken !== undefined && existingToken !== repoToken) ||
      (existingToken === undefined && this.tree.repositoryRevision(uri) !== revision + 1)) {
      throw new Error('The pinned submodule changed while opening.');
    }
    const after = await this.validate(pin, root, generation, session);
    if (!guard() || before.some((value, index) => value !== after[index]) ||
        !this.tree.isCurrentRepository(found, repoToken)) throw new Error('The pinned submodule changed while opening.');
    return { path: target, token: repoToken, repo: found };
  }

  enqueue(operation) {
    const result = this.pending.then(operation);
    this.pending = result.catch(() => {});
    return result;
  }

  beginUserAction() {
    this.interactive++;
    this.restoreEpoch++;
    this.cancelSweep?.();
    this.restoreAgain = false;
    if (this.timer !== undefined) clearTimeout(this.timer);
    if (this.retryTimer !== undefined) clearTimeout(this.retryTimer);
    this.timer = undefined;
    this.retryTimer = undefined;
    this.backend.cancelPinValidation?.();
  }

  endUserAction() {
    this.interactive--;
    if (!this.interactive && !this.disposed && this.visible) {
      this.signature = undefined;
      this.scheduleRestore();
    }
  }

  async save(pins) {
    if (!this.state?.update) throw new Error('VS Code workspace storage is unavailable.');
    const value = { version: 1, pins };
    if (Buffer.byteLength(JSON.stringify(value)) > 1024 * 1024) throw new Error('The workspace submodule pin storage limit was reached.');
    await this.state.update(STATE_KEY, value);
  }

  async waitForOpen(keys) {
    const openings = keys.map(key => this.openings.get(key)?.promise).filter(Boolean);
    if (!openings.length) return;
    throw new Error('VS Code is still opening this submodule. Try removing the pin after the open finishes.');
  }

  pin(root, frames, guard, verifySelection = async () => true) {
    const pin = { root, chain: frames.map(frame => ({ path: frame.path, parent_identity: frame.parent_identity, child_identity: frame.child_identity })) };
    if (!validPin(pin)) return Promise.reject(new Error('Invalid submodule pin.'));
    this.beginUserAction();
    return this.enqueue(async () => {
      if (this.storageError) throw new Error(this.storageError);
      const key = this.key(pin);
      if (!guard()) throw new Error('The selected submodule changed.');
      if (this.pins.some(item => this.key(item) === key)) return;
      if (this.pins.length >= MAX_PINS) throw new Error('The workspace submodule pin limit was reached.');
      if (!await verifySelection() || !guard()) throw new Error('The selected submodule changed.');
      const opened = await this.open(pin, guard);
      if (!await verifySelection() || !guard()) throw new Error('The selected submodule changed.');
      if ([...this.active.values()].some(item => item.path === opened.path && this.tree.isCurrentRepository(item.repo, item.token))) return;
      const next = [...this.pins, pin];
      await this.save(next);
      try {
        const root = this.root(pin);
        if (!root || !guard()) throw new Error('The selected submodule changed while saving the pin.');
        const after = await this.validate(pin, root, this.backend.generation);
        if (!await verifySelection() || !guard() || after.at(-1) !== opened.path || !this.tree.isCurrentRepository(opened.repo, opened.token)) {
          throw new Error('The selected submodule changed while saving the pin.');
        }
      } catch (error) {
        await this.save(this.pins);
        throw error;
      }
      this.pins = next;
      this.version++;
      this.active.set(key, opened);
      this.tree.changed();
    }).finally(() => this.endUserAction());
  }

  unpin(repo, guard = () => true) {
    this.beginUserAction();
    return this.enqueue(async () => {
      const target = canonical(repo?.rootUri?.fsPath);
      if (!target || !guard()) throw new Error('The selected repository is no longer available.');
      const keys = [...this.active].filter(([, value]) => value.path === target).map(([key]) => key);
      if (!keys.length) throw new Error('The selected repository is not pinned.');
      await this.waitForOpen(keys);
      if (!guard()) throw new Error('The selected repository changed while removing the pin.');
      const next = this.pins.filter(pin => !keys.includes(this.key(pin)));
      await this.save(next);
      if (!guard()) {
        await this.save(this.pins);
        throw new Error('The selected repository changed while removing the pin.');
      }
      this.pins = next;
      this.version++;
      for (const key of keys) this.active.delete(key);
      this.tree.changed();
    }).finally(() => this.endUserAction());
  }

  async manage() {
    this.beginUserAction();
    try { return await this.manageSelection(); }
    finally { this.endUserAction(); }
  }

  async manageSelection() {
    if (this.storageError) {
      const version = this.version;
      const generation = this.backend.generation;
      const choice = await this.vscode.window.showWarningMessage(
        'Saved submodule pins are invalid. Resetting removes all saved pins from this workspace.', { modal: true }, 'Reset Pins');
      if (choice !== 'Reset Pins') return;
      return await this.enqueue(async () => {
        if (this.disposed || this.version !== version || this.backend.generation !== generation) {
          throw new Error('The workspace or saved pins changed. Open the pin list again.');
        }
        await this.save([]);
        if (this.disposed || this.version !== version || this.backend.generation !== generation) {
          await this.state.update(STATE_KEY, this.rawState);
          throw new Error('The workspace or saved pins changed. Open the pin list again.');
        }
        this.pins = [];
        this.active.clear();
        this.storageError = undefined;
        this.error = undefined;
        this.rawState = { version: 1, pins: [] };
        this.version++;
        this.tree.changed();
      });
    }
    if (!this.pins.length) {
      await this.vscode.window.showInformationMessage('There are no saved submodule pins.');
      return;
    }
    const version = this.version;
    const generation = this.backend.generation;
    const clean = value => value.replace(/[\x00-\x1f\x7f\u202a-\u202e\u2066-\u2069]/g, ' ').replace(/\$\(/g, '＄(');
    const choices = this.pins.map(pin => ({ key: this.key(pin), label: clean(path.basename(pin.chain.at(-1).path)),
      description: clean(`${pin.root} / ${pin.chain.map(link => link.path).join(' / ')}`),
      detail: this.active.has(this.key(pin)) ? 'Remove this pin' : 'Unavailable pin. Remove it from the workspace.' }));
    const selected = await this.vscode.window.showQuickPick(choices, { placeHolder: 'Choose a saved submodule pin to remove', matchOnDescription: true });
    if (!selected || !choices.includes(selected)) return;
    return await this.enqueue(async () => {
      await this.waitForOpen([selected.key]);
      if (this.disposed || this.version !== version || this.backend.generation !== generation) {
        throw new Error('The workspace or saved pins changed. Open the pin list again.');
      }
      const next = this.pins.filter(pin => this.key(pin) !== selected.key);
      if (next.length === this.pins.length) throw new Error('The selected pin no longer exists.');
      await this.save(next);
      if (this.disposed || this.version !== version || this.backend.generation !== generation) {
        await this.save(this.pins);
        throw new Error('The workspace or saved pins changed. Open the pin list again.');
      }
      this.pins = next;
      this.version++;
      this.active.delete(selected.key);
      this.error = undefined;
      this.signature = undefined;
      this.tree.changed();
    });
  }

  setVisible(visible) {
    if (this.disposed || this.visible === visible) return;
    this.visible = visible;
    this.visibilityEpoch++;
    if (!visible) {
      this.restoreEpoch++;
      this.cancelSweep?.();
      this.backend.cancelPinValidation?.();
      if (this.timer !== undefined) clearTimeout(this.timer);
      if (this.retryTimer !== undefined) clearTimeout(this.retryTimer);
      this.timer = undefined;
      this.retryTimer = undefined;
      return;
    }
    this.signature = undefined;
    this.scheduleRestore();
  }

  scheduleRestore() {
    if (this.disposed || !this.visible || this.interactive || this.storageError || !this.pins.length) return;
    const signature = JSON.stringify([this.backend.generation, this.pins.map(pin => {
      const root = this.root(pin);
      return [this.key(pin), root?.token, root?.path];
    })]);
    if (signature === this.signature) return;
    this.signature = signature;
    if (this.restoreQueued) { this.restoreAgain = true; return; }
    if (this.timer !== undefined) clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      this.timer = undefined;
      this.restoreQueued = true;
      const epoch = this.visibilityEpoch;
      const restoreEpoch = this.restoreEpoch;
      const version = this.version;
      let cancelSweep;
      const cancelled = new Promise(resolve => { cancelSweep = () => resolve({ cancelled: true }); });
      this.cancelSweep = cancelSweep;
      const current = () => !this.disposed && this.visible && !this.interactive && this.visibilityEpoch === epoch &&
        this.restoreEpoch === restoreEpoch && this.version === version;
      (async () => {
        const errors = [];
        let session;
        try {
          for (const pin of [...this.pins]) {
            if (!current()) return;
            const key = this.key(pin);
            try {
              if (!this.root(pin)) throw Object.assign(new Error(`Workspace repository unavailable: ${pin.root}`), { code: 'stale_target' });
              if (!session) {
                const pending = this.backend.openPinValidationSession().then(value => ({ value }), error => ({ error }));
                const opened = await Promise.race([pending, cancelled]);
                if (opened.cancelled) {
                  void pending.then(result => result.value?.close()).catch(error => {
                    if (this.visible && !this.disposed) { this.error = `Could not close pinned submodule validation: ${error.message}`; this.tree.changed(); }
                  });
                  return;
                }
                if (opened.error) throw opened.error;
                session = opened.value;
              }
              if (!current()) return;
              const pending = this.open(pin, current, session).then(value => ({ value }), error => ({ error }));
              const result = await Promise.race([pending, cancelled]);
              if (result.cancelled) return;
              if (result.error) throw result.error;
              const opened = result.value;
              if (!current()) return;
              this.active.set(key, opened);
            } catch (error) {
              if (!current()) return;
              this.active.delete(key);
              errors.push(error?.message || String(error));
              if (!['stale_target', 'outside_workspace', 'repository_error', 'invalid_request', 'open_pending'].includes(error?.code)) {
                this.active.clear();
                break;
              }
            }
          }
        } finally { if (session) await session.close(); }
        if (!current()) return;
        this.error = errors.length ? `Could not restore pinned submodule: ${errors.join('; ')}` : undefined;
        this.tree.changed();
      })().catch(error => {
        if (current()) { this.error = `Could not restore pinned submodule: ${error.message}`; this.tree.changed(); }
      }).finally(() => {
        if (this.cancelSweep === cancelSweep) this.cancelSweep = undefined;
        this.restoreQueued = false;
        if (this.disposed || !this.visible || this.interactive) return;
        if (this.restoreAgain) {
          this.restoreAgain = false;
          this.signature = undefined;
          this.scheduleRestore();
        } else if (this.pins.length && this.retryTimer === undefined) {
          this.retryTimer = setTimeout(() => {
            this.retryTimer = undefined;
            this.signature = undefined;
            this.scheduleRestore();
          }, this.revalidateIntervalMs);
        }
      });
    }, this.restoreDelayMs);
  }

  dispose() {
    this.disposed = true;
    this.restoreEpoch++;
    this.cancelSweep?.();
    this.backend.cancelPinValidation?.();
    if (this.timer !== undefined) clearTimeout(this.timer);
    if (this.retryTimer !== undefined) clearTimeout(this.retryTimer);
    this.subscription?.dispose();
  }
}

module.exports = { SubmodulePins, readPins, validPin };
