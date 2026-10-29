'use strict';

const { validateTheme } = require('./theme');

function themeName(name) { return typeof name === 'string' && name.length > 0 && Buffer.byteLength(name) <= 255 &&
  !name.includes('\0') && !name.includes('/') && !name.includes('\\') && name !== '.' && name !== '..'; }
function persistedName(name) { return typeof name === 'string' && Buffer.byteLength(name) <= 4096; }

class ThemePicker {
  constructor(vscode, backend, controller) {
    this.vscode = vscode;
    this.backend = backend;
    this.controller = controller;
    this.active = undefined;
    this.disposed = false;
    this.reconcileVersion = 0;
    this.reconcilePending = false;
    this.reconciling = false;
    this.subscription = backend.onDidChange(state => {
      if (state.status === 'initializing' || state.status === 'error') this.active?.cancel();
      if (state.status === 'ready' && this.reconcilePending) this.reconcile();
    });
  }

  scheduleReconcile(workerClosed) {
    Promise.resolve(workerClosed).then(() => {
      if (this.disposed) return;
      this.reconcileVersion++;
      this.reconcilePending = true;
      this.reconcile();
    });
  }

  async reconcile() {
    if (this.disposed || this.reconciling || !this.reconcilePending || this.backend.state?.status !== 'ready') return;
    this.reconciling = true;
    try {
      while (!this.disposed && this.reconcilePending && this.backend.state?.status === 'ready') {
        const generation = this.backend.generation;
        const version = this.reconcileVersion;
        let value, error;
        try { value = validateTheme(await this.backend.theme()); }
        catch (failure) { error = `Gitpane theme: ${failure.message}`; }
        if (this.disposed) return;
        if (generation !== this.backend.generation || version !== this.reconcileVersion) continue;
        this.reconcilePending = false;
        const active = this.active;
        if (active?.baseline && !active.cancelled && this.controller.previewCurrent(active.baseline)) {
          active.baseline.value = value;
          active.baseline.error = error;
          if (!active.previewing) this.controller.applyPreview(active.baseline, value, error);
        } else {
          const baseline = this.controller.beginPreview();
          this.controller.applyPreview(baseline, value, error);
        }
        if (error) this.vscode.window.showErrorMessage(`Gitpane could not reload the theme after the save: ${error}`);
      }
    } finally { this.reconciling = false; }
  }

  async open() {
    if (this.disposed || this.active) return;
    const session = { generation: this.backend.generation, cancelled: false };
    this.active = session;
    session.cancel = () => { session.cancelled = true; session.picker?.hide(); };
    const live = () => !this.disposed && this.active === session && !session.cancelled &&
      session.generation === this.backend.generation && this.controller.previewCurrent(session.baseline);
    try {
      if (!this.controller.value) await this.controller.retry();
      if (this.disposed || session.cancelled || session.generation !== this.backend.generation) return;
      session.baseline = this.controller.beginPreview();
      const listing = await this.backend.themesList();
      if (!live()) return;
      if (!listing || !persistedName(listing.current) || !persistedName(listing.persisted) || !Array.isArray(listing.names) ||
        listing.names.length > 4096 || listing.names.some(name => !themeName(name)) || new Set(listing.names).size !== listing.names.length) {
        throw new Error('Gitpane returned an invalid theme list.');
      }
      const picker = this.vscode.window.createQuickPick();
      session.picker = picker;
      picker.title = 'Gitpane Theme';
      picker.placeholder = 'Choose a theme to preview';
      picker.items = listing.names.map(name => ({ label: name, description: name === listing.current ? 'Current' : undefined, name }));
      let sequence = 0;
      let preview;
      let previewItem;
      let saving = false;
      let completed = false;
      const validItem = item => picker.items.includes(item);
      const select = item => {
        const current = ++sequence;
        previewItem = item;
        if (!validItem(item)) { preview = undefined; return; }
        preview = (async () => {
          try {
            const response = await this.backend.themePreview(item.name);
            if (!live() || current !== sequence) return undefined;
            if (!response || !/^[a-f0-9]{64}$/.test(response.preview_token || '')) throw new Error('Gitpane returned an invalid theme preview.');
            const theme = validateTheme(response.theme);
            if (theme.name !== item.name) throw new Error('Gitpane returned a different theme than requested.');
            session.previewing = true;
            this.controller.applyPreview(session.baseline, theme, undefined);
            picker.validationMessage = undefined;
            return { item, theme, token: response.preview_token };
          } catch (error) {
            if (live() && current === sequence) {
              session.previewing = false;
              this.controller.applyPreview(session.baseline, session.baseline.value, session.baseline.error);
              picker.validationMessage = `Could not preview theme: ${error.message}`;
            }
            return undefined;
          }
        })();
      };
      await new Promise(resolve => {
        let closed = false;
        const finish = () => {
          if (closed) return;
          closed = true;
          session.cancelled = true;
          for (const listener of listeners) listener.dispose();
          picker.dispose();
          resolve();
        };
        const listeners = [picker.onDidChangeActive(items => { if (!saving) select(items[0]); }),
          picker.onDidAccept(async () => {
            if (saving || !live()) return;
            const item = picker.selectedItems[0] || picker.activeItems[0];
            if (!validItem(item)) return;
            if (!preview || item !== previewItem) select(item);
            saving = true;
            picker.busy = true;
            picker.enabled = false;
            const chosen = await preview;
            if (!live()) { saving = false; return; }
            if (!chosen || chosen.item !== item) { saving = false; picker.busy = false; picker.enabled = true; return; }
            session.accepted = true;
            try {
              const result = await this.backend.themeSave(item.name, listing.persisted, chosen.token, () => live());
              let saved;
              try {
                saved = validateTheme(result?.theme);
                if (saved.name !== item.name) throw new Error('Gitpane returned a different saved theme.');
              } catch (error) {
                throw Object.assign(new Error(`The save response was invalid: ${error.message}`), { code: 'outcome_unknown' });
              }
              if (live()) this.controller.applyPreview(session.baseline, saved, undefined);
              this.scheduleReconcile();
            } catch (error) {
              if (error.code === 'outcome_unknown') {
                if (live()) this.controller.applyPreview(session.baseline, undefined, 'Gitpane theme save outcome is unknown. Waiting to reload the saved theme.');
                this.scheduleReconcile(error.workerClosed);
              } else if (live()) this.controller.applyPreview(session.baseline, session.baseline.value, session.baseline.error);
              if (!this.disposed) this.vscode.window.showErrorMessage(`Gitpane could not save the theme: ${error.message}`);
            } finally { completed = true; picker.hide(); finish(); }
          }),
          picker.onDidHide(() => {
            if (!session.accepted && live()) this.controller.applyPreview(session.baseline, session.baseline.value, session.baseline.error);
            if (session.accepted && !completed) return;
            finish();
          })];
        picker.show();
      });
    } catch (error) {
      if (session.baseline && live()) this.controller.applyPreview(session.baseline, session.baseline.value, session.baseline.error);
      if (!session.cancelled && !this.disposed && session.generation === this.backend.generation) {
        this.vscode.window.showErrorMessage(`Gitpane theme picker: ${error.message}`);
      }
    } finally { if (this.active === session) this.active = undefined; }
  }

  dispose() { this.disposed = true; this.active?.cancel(); this.subscription.dispose(); }
}

module.exports = { ThemePicker };
