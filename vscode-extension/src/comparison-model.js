'use strict';

const { relativeFile } = require('./change-pager');
const oid = value => typeof value === 'string' && /^[a-f0-9]{40}$/.test(value);
const identity = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);

class ComparisonModel {
  constructor(service, repo, current) {
    this.service = service;
    this.repo = repo;
    this.current = current;
    this.from = undefined;
    this.to = undefined;
    this.result = undefined;
    this.mode = 'endpoints';
    this.generation = 0;
  }

  invalidate() {
    this.generation++;
    this.service.cancel();
    this.result = undefined;
  }

  cancelPending() { this.generation++; this.service.cancel(); }

  async resolve(revision, generation) {
    if (typeof revision !== 'string' || !revision || revision.length > 1024) throw new Error('Enter a revision name or commit ID.');
    const guard = () => this.generation === generation && this.current();
    const result = await this.service.request('resolve_revision', this.repo, { revision }, guard);
    if (!guard() || !oid(result?.oid) || !identity(result?.repo_identity)) throw new Error('Revision or repository is no longer available.');
    return { revision, oid: result.oid, identity: result.repo_identity, moved: false };
  }

  async set(which, revision) {
    if (!['from', 'to'].includes(which)) throw new Error('Invalid comparison endpoint.');
    this.invalidate();
    const generation = this.generation;
    const endpoint = await this.resolve(revision, generation);
    const other = this[which === 'from' ? 'to' : 'from'];
    if (other && other.identity !== endpoint.identity) throw new Error('Repository changed while selecting revisions.');
    this[which] = endpoint;
  }

  swap() { this.invalidate(); [this.from, this.to] = [this.to, this.from]; }
  clear() { this.invalidate(); this.from = undefined; this.to = undefined; }
  setMode(mode) {
    if (!['endpoints', 'common_ancestor'].includes(mode)) throw new Error('Invalid comparison mode.');
    if (this.mode !== mode) { this.invalidate(); this.mode = mode; }
  }

  async refresh() {
    this.invalidate();
    const generation = this.generation;
    const from = this.from && await this.resolve(this.from.revision, generation);
    const to = this.to && await this.resolve(this.to.revision, generation);
    if (from && to && from.identity !== to.identity) throw new Error('Repository changed while resolving revisions.');
    this.from = from;
    this.to = to;
  }

  async checkRefs() {
    this.generation++;
    this.service.cancel();
    const generation = this.generation;
    for (const which of ['from', 'to']) {
      const endpoint = this[which];
      if (!endpoint) continue;
      try {
        const latest = await this.resolve(endpoint.revision, generation);
        if (latest.identity !== endpoint.identity) throw new Error('Repository identity changed.');
        this[which] = { ...endpoint, moved: latest.oid !== endpoint.oid, error: undefined };
      } catch (error) {
        if (generation !== this.generation || !this.current()) throw error;
        this[which] = { ...endpoint, error: error.message };
      }
    }
  }

  async compare() {
    if (!this.from || !this.to) throw new Error('Select both comparison endpoints.');
    this.invalidate();
    const generation = this.generation;
    const guard = () => this.generation === generation && this.current();
    let offset = 0, first;
    const files = [];
    do {
      const page = await this.service.request('compare_revisions', this.repo, {
        from: this.from.oid, to: this.to.oid, common_ancestor: this.mode === 'common_ancestor', offset
      }, guard);
      if (!guard() || page?.repo_identity !== this.from.identity || page.repo_identity !== this.to.identity ||
          !oid(page.base_oid) || (this.mode === 'endpoints' && page.base_oid !== this.from.oid) ||
          page.to_oid !== this.to.oid || !Number.isSafeInteger(page.total) || page.total < 0 ||
          !Array.isArray(page.files) || page.files.length !== Math.min(256, page.total - offset) ||
          page.next !== (offset + page.files.length < page.total ? offset + 256 : null) ||
          (first && (page.base_oid !== first.base_oid || page.total !== first.total)) ||
          page.files.some(file => !file || !relativeFile(file.path) || !['A', 'D', 'M', 'R', '?'].includes(file.status) ||
            (file.status === 'R' && !relativeFile(file.old_path)) || typeof file.is_submodule !== 'boolean')) {
        throw new Error('Gitpane returned an invalid or stale comparison.');
      }
      if (page.total > 100000) throw new Error('Comparison exceeds the supported limit of 100,000 files.');
      first ||= page;
      files.push(...page.files.map(file => ({ ...file, comparisonOffset: offset })));
      offset = page.next;
    } while (offset !== null);
    this.result = { ...first, files };
    return this.result;
  }

  dispose() { this.invalidate(); }
}

module.exports = { ComparisonModel };
