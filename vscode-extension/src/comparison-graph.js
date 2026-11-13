'use strict';

const { ComparisonService } = require('./comparison-service');
const { ComparisonModel } = require('./comparison-model');
const { ComparisonController } = require('./comparison-controller');
const { groups } = require('./model');

class ComparisonGraph {
  constructor(provider, vscode, repo, token, current, context) {
    this.provider = provider;
    this.vscode = vscode;
    this.repo = repo;
    this.current = current;
    this.context = context;
    this.epoch = 0;
    this.loading = false;
    this.error = undefined;
    this.files = [];
    this.complete = false;
    this.browseGeneration = 0;
    this.refService = new ComparisonService(provider.backend);
    this.model = new ComparisonModel(new ComparisonService(provider.backend), repo.rootUri.fsPath, current);
    this.controller = new ComparisonController({ vscode, model: this.model, backend: provider.backend,
      tree: provider.tree, links: provider.links, repo, token, current,
      validate: () => provider.validateGraph(repo), actionGuard: () => this.actionGuard(), context });
  }

  actionGuard() {
    const panel = this.provider.panel, generation = this.provider.graphGeneration;
    return () => this.current() && !!panel && this.provider.panel === panel && panel.visible !== false &&
      this.provider.interactive && this.provider.graphGeneration === generation &&
      (!this.provider.isCurrentRepo || this.provider.isCurrentRepo(this.repo));
  }

  suspend() {
    this.epoch++;
    this.cancelBrowse();
    this.loading = false;
    this.model.cancelPending();
    this.controller.suspend();
  }

  publish() {
    const repo = this.provider.currentRepo?.() || this.repo;
    const head = repo.state?.HEAD;
    const externalStatus = this.externalStatusUnavailable ? undefined : this.externalStatus || this.context?.status?.();
    const state = repo.state;
    const trackedGroups = [state?.workingTreeChanges, state?.indexChanges, state?.mergeChanges];
    const dirty = typeof externalStatus?.is_dirty === 'boolean' ? externalStatus.is_dirty :
      trackedGroups.every(Array.isArray) ? groups(state).length > 0 : undefined;
    const branch = externalStatus?.branch && externalStatus.branch !== 'HEAD' ? externalStatus.branch : head?.name;
    const commit = externalStatus?.head_oid || head?.commit;
    this.provider.post({ type: 'comparisonState', repoName: this.provider.repoName, repoPath: this.repo.rootUri.fsPath,
      head: branch || (commit ? `Detached ${commit.slice(0, 8)}` : undefined), dirty,
      from: this.model.from, to: this.model.to, mode: this.model.mode,
      files: this.files, hasResult: this.complete, loading: this.loading, error: this.error });
  }

  async change(action) {
    const epoch = ++this.epoch;
    this.cancelBrowse();
    this.controller.clearFiles();
    this.files = [];
    this.complete = false;
    this.loading = true;
    this.error = undefined;
    this.publish();
    const active = this.actionGuard();
    try {
      if (!active() || !await this.provider.validateGraph(this.repo)) throw new Error('Repository is no longer available.');
      if (epoch !== this.epoch || !active()) return;
      await action();
      if (epoch !== this.epoch || !active()) return;
      if (this.model.from && this.model.to) {
        const files = await this.controller.compare();
        if (epoch !== this.epoch || !active()) return;
        this.files = files;
        this.complete = true;
      }
    } catch (error) {
      if (epoch === this.epoch) this.error = error.message;
    } finally {
      if (epoch === this.epoch) { this.loading = false; this.publish(); }
    }
  }

  async pick(which) {
    if (!['from', 'to'].includes(which)) return;
    this.cancelBrowse();
    const epoch = this.epoch, generation = this.browseGeneration, active = this.actionGuard();
    const cancellation = new this.vscode.CancellationTokenSource();
    this.pickerToken = cancellation;
    let revision;
    try { revision = await this.vscode.window.showInputBox({ title: `Select ${which === 'from' ? 'From' : 'To'} revision`,
      prompt: 'Enter a branch, tag, or commit ID', value: this.model[which]?.revision || '' }, cancellation.token); }
    finally { if (this.pickerToken === cancellation) { this.pickerToken = undefined; cancellation.dispose(); } }
    if (revision === undefined || epoch !== this.epoch || generation !== this.browseGeneration || !active()) return;
    await this.change(() => this.model.set(which, revision.trim()));
  }

  async browse(which) {
    if (!['from', 'to'].includes(which)) return;
    this.cancelBrowse();
    const epoch = this.epoch, generation = this.browseGeneration, active = this.actionGuard();
    const current = () => epoch === this.epoch && generation === this.browseGeneration && active();
    try {
      let offset = 0, snapshot;
      const refs = [];
      do {
        const page = await this.refService.request('revision_refs', this.repo.rootUri.fsPath, { offset }, current);
        if (!current() || !/^[a-f0-9]{64}$/.test(page?.revision || '') || !/^[a-f0-9]{64}$/.test(page?.repo_identity || '') ||
            !Number.isSafeInteger(page.total) || page.total < 0 || page.total > 100000 || !Array.isArray(page.refs) ||
            page.refs.length !== Math.min(256, page.total - offset) ||
            page.next !== (offset + page.refs.length < page.total ? offset + 256 : null) ||
            page.refs.some(ref => !ref || typeof ref.name !== 'string' || !/^refs\/(heads|remotes|tags)\/.+/.test(ref.name) ||
              (ref.oid !== null && !/^[a-f0-9]{40}$/.test(ref.oid))) ||
            (snapshot && (snapshot.revision !== page.revision || snapshot.total !== page.total || snapshot.repo_identity !== page.repo_identity))) {
          throw new Error('Gitpane returned an invalid or changed ref list.');
        }
        snapshot ||= page;
        refs.push(...page.refs);
        offset = page.next;
      } while (offset !== null);
      if (!current()) return;
      if (!refs.length) { await this.vscode.window.showInformationMessage('No branches or tags are available.'); return; }
      const choices = refs.map(ref => ({ label: ref.name.replace(/^refs\/(heads|remotes|tags)\//, ''),
        description: `${ref.name.startsWith('refs/tags/') ? 'Tag' : ref.name.startsWith('refs/remotes/') ? 'Remote branch' : 'Branch'}${ref.oid ? '' : ' · unavailable'}`,
        revision: ref.name, oid: ref.oid }));
      const cancellation = new this.vscode.CancellationTokenSource();
      this.pickerToken = cancellation;
      let selected;
      try { selected = await this.vscode.window.showQuickPick(choices,
        { title: `Select ${which === 'from' ? 'From' : 'To'} ref`, matchOnDescription: true }, cancellation.token); }
      finally { if (this.pickerToken === cancellation) { this.pickerToken = undefined; cancellation.dispose(); } }
      if (!choices.includes(selected) || !current()) return;
      const latest = await this.refService.request('revision_refs', this.repo.rootUri.fsPath, { offset: 0 }, current);
      if (!current()) return;
      if (latest?.revision !== snapshot.revision || latest.repo_identity !== snapshot.repo_identity) throw new Error('Refs changed. Open the picker again.');
      if (!selected.oid) throw new Error('The selected ref does not point to an available commit.');
      await this.change(async () => {
        await this.model.set(which, selected.revision);
        if (this.model[which]?.oid !== selected.oid || this.model[which]?.identity !== snapshot.repo_identity) {
          this.model[which] = undefined;
          throw new Error('The selected ref or repository changed. Open the picker again.');
        }
      });
    } catch (error) {
      if (current()) { this.error = error.message; this.publish(); }
    }
  }

  cancelBrowse() {
    this.browseGeneration++;
    this.refService.cancel();
    if (this.pickerToken) { this.pickerToken.cancel(); this.pickerToken.dispose(); this.pickerToken = undefined; }
  }

  async setRow(which, oid, labelIndex, guard = () => true) {
    const row = this.provider.rows.get(oid);
    if (!['from', 'to'].includes(which) || !row || row.collapsed || !this.provider.interactive || !guard()) return;
    let revision = row.oid;
    if (labelIndex !== undefined) {
      const label = Number.isSafeInteger(labelIndex) && row.labels?.[labelIndex];
      if (!label || label.is_stash || typeof label.name !== 'string') return;
      revision = `${label.is_tag ? 'refs/tags/' : label.is_remote ? 'refs/remotes/' : 'refs/heads/'}${label.name}`;
    }
    await this.change(async () => {
      if (!guard() || this.provider.rows.get(oid) !== row || !this.provider.interactive) throw new Error('Commit is no longer in the graph.');
      await this.model.set(which, revision);
      if (!guard() || this.provider.rows.get(oid) !== row || this.model[which]?.oid !== oid) {
        this.model[which] = undefined;
        throw new Error('The selected revision moved. Refresh the graph and try again.');
      }
    });
  }

  async receive(message) {
    if (!this.actionGuard()()) return true;
    switch (message.type) {
      case 'comparisonPick': await this.pick(message.which); return true;
      case 'comparisonBrowse': await this.browse(message.which); return true;
      case 'comparisonSetSelected': await this.setRow(message.which, this.provider.selected); return true;
      case 'comparisonSelect': {
        if (!this.provider.rows.has(message.oid) || this.provider.rows.get(message.oid).collapsed) return true;
        const pending = this.provider.pendingSelection;
        if (!this.model.from && pending?.oid === message.anchor && pending.selection === this.provider.selectionEpoch) {
          const epoch = this.epoch, active = this.actionGuard();
          await pending.promise;
          if (epoch !== this.epoch || !active() || this.provider.selectionEpoch !== pending.selection || this.provider.selected !== pending.oid) return true;
        }
        const anchor = this.provider.rows.get(message.anchor);
        const target = this.provider.rows.get(message.oid);
        if (!this.model.from && anchor && !anchor.collapsed && anchor !== target && this.provider.selected === anchor.oid) {
          await this.change(async () => {
            if (this.provider.rows.get(anchor.oid) !== anchor || this.provider.rows.get(target.oid) !== target ||
                this.provider.selected !== anchor.oid) throw new Error('Graph selection changed.');
            await this.model.set('from', anchor.oid);
            if (this.provider.rows.get(anchor.oid) !== anchor || this.provider.rows.get(target.oid) !== target ||
                this.provider.selected !== anchor.oid) throw new Error('Graph selection changed.');
            await this.model.set('to', target.oid);
          });
          return true;
        }
        const which = !this.model.from || this.model.to ? 'from' : 'to';
        if (this.model.from && this.model.to) this.model.clear();
        await this.setRow(which, message.oid); return true;
      }
      case 'comparisonSwap': await this.change(() => this.model.swap()); return true;
      case 'comparisonClear': await this.change(() => this.model.clear()); return true;
      case 'comparisonRefresh': await this.change(() => this.model.refresh()); return true;
      case 'comparisonMode':
        if (['endpoints', 'common_ancestor'].includes(message.mode)) await this.change(() => this.model.setMode(message.mode));
        return true;
      case 'openComparisonFile':
        try { await this.controller.open(message.id); }
        catch (error) { await this.vscode.window.showErrorMessage(`Gitpane could not open the comparison: ${error.message}`); }
        return true;
      default: return false;
    }
  }

  async checkRefs() {
    if (this.loading || (!this.model.from && !this.model.to)) return;
    const epoch = this.epoch;
    try { await this.model.checkRefs(); }
    catch (error) { if (epoch === this.epoch) this.error = error.message; }
    if (epoch === this.epoch) this.publish();
  }

  async refreshExternalStatus() {
    const active = this.actionGuard();
    if (!active()) return;
    try {
      const status = await this.provider.backend.externalStatus(this.repo.rootUri.fsPath);
      if (!active()) return;
      if (typeof status?.is_dirty !== 'boolean' || typeof status.branch !== 'string' ||
          status.repo_identity !== this.context?.status?.()?.repo_identity) throw new Error('Outside worktree status is invalid or stale.');
      this.externalStatus = status;
      this.externalStatusUnavailable = false;
      this.publish();
    } catch (error) {
      if (active()) { this.externalStatusUnavailable = true; this.publish(); }
      throw error;
    }
  }

  dispose() { this.epoch++; this.cancelBrowse(); this.refService.dispose(); this.controller.dispose(); }
}

module.exports = { ComparisonGraph };
