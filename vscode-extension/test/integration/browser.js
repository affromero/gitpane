'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { pathToFileURL } = require('node:url');

const extension = path.resolve(__dirname, '../..');
const browser = process.env.CHROME_EXECUTABLE || (process.platform === 'darwin'
  ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' : 'google-chrome');
const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-browser-test-'));
const stylesheet = fs.readFileSync(path.join(extension, 'src/webview/styles.css'), 'utf8');
const client = fs.readFileSync(path.join(extension, 'src/webview/view.js'), 'utf8');
const themeClient = fs.readFileSync(path.join(extension, 'src/theme-webview.js'), 'utf8');
const snapshot = {
  type: 'snapshot', errors: [], loading: false,
  repositories: [
    { id: 'api', name: 'api-service', branch: 'main', dirty: true, live: true, pinned: true, ahead: 2, behind: 1,
      groups: [
        { id: 'working', label: 'Unstaged', files: [
          { id: 'file1', status: 'M', path: 'src/server.rs' },
          { id: 'unsafe', status: '?', path: '</script><img src=x onerror="window.injected=true">' }
        ] },
        { id: 'index', label: 'Staged', files: [{ id: 'file2', status: 'M', path: 'src/server.rs' }] }
      ] },
    { id: 'web', name: 'web-app', branch: 'feature/ui', dirty: false, ahead: 0, behind: 0, groups: [] },
    { id: 'docs', name: 'docs', branch: 'docs/edit', dirty: true, ahead: 0, behind: 0,
      groups: [{ id: 'working', label: 'Unstaged', files: [{ id: 'file3', status: 'M', path: 'README.md' }] }] },
    { id: 'sync', name: 'sync', branch: 'feature/sync', dirty: true, ahead: 12, behind: 3,
      groups: [{ id: 'working', label: 'Unstaged', files: [{ id: 'file4', status: 'M', path: 'sync.rs' }] }] },
    { id: 'offline', name: 'offline', branch: 'main', dirty: false, ahead: 0, behind: 0,
      statusUnavailable: true, groups: [] }
  ]
};
const encodedSnapshot = Buffer.from(JSON.stringify(snapshot)).toString('base64');
const scenario = String.raw`
(async () => {
  const checks = [];
  const check = (name, condition) => {
    if (!condition) throw new Error(name);
    checks.push(name);
  };
  const tick = () => new Promise(resolve => setTimeout(resolve, 30));
  const items = () => [...document.querySelectorAll('[role="treeitem"]')];
  const luminance = color => {
    const channels = color.match(/\d+/g).slice(0, 3).map(value => {
      const channel = Number(value) / 255;
      return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
    });
    return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
  };
  const contrast = (first, second) => {
    const a = luminance(first);
    const b = luminance(second);
    return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
  };
  const snapshot = JSON.parse(atob('SNAPSHOT_BASE64'));
  try {
    window.postMessage(snapshot, '*');
    await tick();
    const themed = JSON.parse(JSON.stringify(snapshot));
    themed.theme = { name: 'custom', values: { 'repo-list-stash': 'rgb(255, 0, 120)',
      'repo-list-worktree-count': 'rgb(240, 150, 0)', 'repo-list-repo-name': 'rgb(0, 180, 180)',
      'repo-list-dirty-marker': 'rgb(220, 180, 0)', 'repo-list-branch': 'rgb(20, 180, 200)',
      'repo-list-file-count': 'rgb(220, 180, 0)',
      'repo-list-live': 'rgb(30, 190, 130)',
      'repo-list-worktree-subtree-icon': 'rgb(160, 80, 240)', 'file-list-regular-path': 'rgb(0, 180, 0)',
      'file-list-submodule-path': 'rgb(255, 100, 0)', 'file-list-submodule-branch': 'rgb(20, 180, 200)' }, lanes: [], authors: [] };
    themed.repositories[1].live = true;
    themed.repositories[0].branch = 'feature/colors';
    window.postMessage(themed, '*');
    await tick();
    check('workspace semantic theme applies under CSP',
      !!document.documentElement.style.getPropertyValue('--gitpane-repo-list-stash') &&
      document.documentElement.style.getPropertyValue('--gitpane-repo-list-stash') !==
        document.documentElement.style.getPropertyValue('--gitpane-repo-list-worktree-count'));
    const webName = items().find(item => item.getAttribute('aria-label').includes('web-app')).querySelector('.name');
    check('repository names use native semantic color', getComputedStyle(webName).color ===
      document.documentElement.style.getPropertyValue('--gitpane-repo-list-repo-name'));
    check('live session uses native semantic color', getComputedStyle(webName.closest('.row').querySelector('.live')).color ===
      document.documentElement.style.getPropertyValue('--gitpane-repo-list-live'));
    webName.closest('[role="treeitem"]').focus();
    let coloredRepo = items().find(item => item.getAttribute('aria-label').includes('api-service'));
    const semanticSelectors = ['.dirty', '.name', '.branch', '.count'];
    const unselectedColors = semanticSelectors.map(selector => getComputedStyle(coloredRepo.querySelector(selector)).color);
    coloredRepo.focus();
    for (const [index, selector] of semanticSelectors.entries()) {
      check('selection preserves semantic color for ' + selector,
        getComputedStyle(coloredRepo.querySelector(selector)).color === unselectedColors[index]);
    }
    window.postMessage(themed, '*');
    await tick();
    coloredRepo = items().find(item => item.getAttribute('aria-label').includes('api-service'));
    check('refresh preserves row selection', coloredRepo.getAttribute('aria-selected') === 'true');
    document.getElementById('start-search').focus();
    for (const [index, selector] of semanticSelectors.entries()) {
      check('refresh and focus changes preserve semantic color for ' + selector,
        getComputedStyle(coloredRepo.querySelector(selector)).color === unselectedColors[index]);
    }
    const badTheme = JSON.parse(JSON.stringify(themed));
    badTheme.theme.values['repo-list-stash'] = 'red; display: none';
    window.postMessage(badTheme, '*');
    await tick();
    check('invalid workspace theme is visible as an error', [...document.querySelectorAll('[role="alert"]')]
      .some(node => node.textContent.includes('invalid theme')));
    window.postMessage(snapshot, '*');
    await tick();
    const root = document.querySelector('[role="tree"]');
    check('tree label', root.getAttribute('aria-label') === 'Gitpane repositories');
    for (const width of [500, 300, 210]) {
      root.style.width = width + 'px';
      const rows = [...document.querySelectorAll('#repositories > .treeitem > .repo-row')];
      const branches = rows.map(row => row.querySelector('.branch').getBoundingClientRect().left);
      check('aligned branch columns at ' + width + 'px', branches.every(left => Math.abs(left - branches[0]) < 1));
      const marker = rows[0].querySelector('.dirty');
      const markerText = document.createRange();
      markerText.selectNodeContents(marker);
      check('dirty marker has a visible gap at ' + width + 'px',
        rows[0].querySelector('.name').getBoundingClientRect().left - markerText.getBoundingClientRect().right >= 4);
      for (const row of rows) {
        const tail = row.querySelector('.tail');
        const bounds = tail.getBoundingClientRect();
        for (const indicator of tail.children) {
          const indicatorBounds = indicator.getBoundingClientRect();
          check('status indicator visible at ' + width + 'px',
            indicatorBounds.left >= bounds.left - 1 && indicatorBounds.right <= bounds.right + 1);
        }
      }
    }
    root.style.width = '';
    const frame = document.createElement('iframe');
    frame.style.cssText = 'position:fixed;left:-9999px;width:210px;height:650px;border:0';
    document.body.appendChild(frame);
    const frameDocument = frame.contentDocument;
    frameDocument.head.appendChild(document.querySelector('style').cloneNode(true));
    frameDocument.body.className = document.body.className;
    frameDocument.body.appendChild(root.cloneNode(true));
    check('narrow viewport triggers compact layout', frame.contentWindow.innerWidth <= 230);
    const narrowRows = [...frameDocument.querySelectorAll('#repositories > .treeitem > .repo-row')];
    const narrowBranches = narrowRows.map(row => row.querySelector('.branch').getBoundingClientRect().left);
    check('aligned branch columns in narrow viewport', narrowBranches.every(left => Math.abs(left - narrowBranches[0]) < 1));
    for (const row of narrowRows) {
      const tail = row.querySelector('.tail');
      const bounds = tail.getBoundingClientRect();
      for (const indicator of tail.children) {
        const indicatorBounds = indicator.getBoundingClientRect();
        check('status indicator visible in narrow viewport',
          indicatorBounds.left >= bounds.left - 1 && indicatorBounds.right <= bounds.right + 1);
      }
    }
    frame.remove();
    let repo = items().find(item => item.getAttribute('aria-label').includes('api-service'));
    check('repository status and branch', repo.textContent.includes('*') && repo.textContent.includes('main'));
    check('live session is visible and accessible', repo.textContent.includes('◉') && repo.getAttribute('aria-label').includes('live session'));
    check('pinned submodule is visible and accessible', repo.textContent.includes('⌖') && repo.getAttribute('aria-label').includes('pinned submodule'));
    check('ahead and behind', repo.textContent.includes('↑2') && repo.textContent.includes('↓1'));
    check('unique file count', repo.textContent.includes('[2]'));
    check('collapsed by default', repo.getAttribute('aria-expanded') === 'false');
    const graphButton = repo.querySelector('.graph-action');
    check('graph action has an accessible name', graphButton.getAttribute('aria-label') === 'Open commit graph for api-service');
    graphButton.click();
    check('graph action selects the repository without expanding',
      window.posted.some(message => message.type === 'openGraph' && message.id === 'api') && repo.getAttribute('aria-expanded') === 'false');
    const repoMenu = repo.querySelector('.menu-action');
    check('repository actions have an accessible name', repoMenu.getAttribute('aria-label') === 'Actions for api-service');
    repoMenu.click();
    check('repository actions do not expand the row',
      window.posted.some(message => message.type === 'menu' && message.kind === 'repo' && message.id === 'api') &&
      repo.getAttribute('aria-expanded') === 'false');
    repoMenu.focus();
    repoMenu.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    check('action keyboard activation does not expand the row', repo.getAttribute('aria-expanded') === 'false');
    window.postMessage(snapshot, '*');
    await tick();
    check('focused action survives refresh', document.activeElement.getAttribute('aria-label') === 'Actions for api-service');
    const badges = JSON.parse(JSON.stringify(snapshot));
    Object.assign(badges.repositories[0], { stashCount: 2, worktreeCount: 1, hasDirtySubmodules: true, hasUnpushedSubmodules: true });
    window.postMessage(badges, '*');
    await tick();
    repo = items().find(item => item.getAttribute('aria-label').includes('api-service'));
    check('repository indicators are visible', ['$2', '⎇1', '◈', '⇡'].every(value => repo.textContent.includes(value)));
    check('repository indicators have accessible meaning', ['2 stashes', '1 worktree', 'dirty submodules', 'submodules need attention']
      .every(value => repo.getAttribute('aria-label').includes(value)));
    badges.repositories[0].statusUnavailable = true;
    window.postMessage(badges, '*');
    await tick();
    repo = items().find(item => item.getAttribute('aria-label').includes('api-service'));
    check('unavailable status does not show stale counts', repo.getAttribute('aria-label').includes('status unavailable') &&
      !repo.textContent.includes('$2') && !repo.textContent.includes('⎇1'));
    const withStashes = JSON.parse(JSON.stringify(snapshot));
    Object.assign(withStashes.repositories[1], { stashCount: 2, stashes: [
      { id: 'stash-a', index: 0, message: 'WIP on feature: save work' },
      { id: 'stash-b', index: 1, message: '<img src=x onerror="window.injected=true">' }
    ] });
    window.postMessage(withStashes, '*');
    await tick();
    let stashRepo = items().find(item => item.getAttribute('aria-label').includes('web-app'));
    check('repository with stashes can expand', stashRepo.getAttribute('aria-expanded') === 'false');
    let stashButton = stashRepo.querySelector('.stash-action');
    check('stash toggle is accessible', stashButton.getAttribute('aria-label') === 'Expand stashes for web-app' &&
      stashButton.getAttribute('aria-expanded') === 'false');
    stashButton.focus();
    check('selected stash action keeps its semantic color', getComputedStyle(stashButton).color ===
      document.documentElement.style.getPropertyValue('--gitpane-repo-list-stash'));
    stashButton.click();
    stashRepo = items().find(item => item.getAttribute('aria-label').includes('web-app'));
    stashButton = stashRepo.querySelector('.stash-action');
    check('stash badge expands repository and stash details', stashRepo.getAttribute('aria-expanded') === 'true' &&
      stashButton.getAttribute('aria-expanded') === 'true' && document.activeElement === stashButton);
    let stashGroup = items().find(item => item.getAttribute('aria-label') === 'Stashes, 2 stashes');
    check('stash group exposes expanded state', stashGroup.getAttribute('aria-expanded') === 'true');
    check('stash details show index and message', items().some(item => item.getAttribute('aria-label') ===
      'stash@{0}, WIP on feature: save work'));
    check('stash message is rendered as text', items().some(item => item.textContent.includes('<img src=x')) &&
      !document.querySelector('img') && !window.injected);
    window.postMessage(withStashes, '*');
    await tick();
    stashGroup = items().find(item => item.getAttribute('aria-label') === 'Stashes, 2 stashes');
    check('stash expansion survives snapshots', stashGroup.getAttribute('aria-expanded') === 'true');
    stashGroup.querySelector('.row').click();
    stashGroup = items().find(item => item.getAttribute('aria-label') === 'Stashes, 2 stashes');
    check('stash group collapses on click', stashGroup.getAttribute('aria-expanded') === 'false' &&
      !items().some(item => item.getAttribute('aria-label').startsWith('stash@{0}')));
    stashGroup.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
    check('keyboard expands stash details', items().some(item => item.getAttribute('aria-label').startsWith('stash@{0}')));
    stashGroup = items().find(item => item.getAttribute('aria-label') === 'Stashes, 2 stashes');
    stashGroup.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
    check('keyboard collapses stash details', !items().some(item => item.getAttribute('aria-label').startsWith('stash@{0}')));
    stashRepo = items().find(item => item.getAttribute('aria-label').includes('web-app'));
    stashRepo.querySelector('.row').click();
    const withWorktree = JSON.parse(JSON.stringify(snapshot));
    withWorktree.launcherKeys = ['j', 'w', 'p', 'q', '=', '?'];
    withWorktree.theme = themed.theme;
    Object.assign(withWorktree.repositories[0], { stashCount: 22, worktreeCount: 23,
      hasDirtySubmodules: true, hasUnpushedSubmodules: true });
    Object.assign(withWorktree.repositories[4], { name: 'repository-with-a-very-long-display-name',
      branch: 'feature/a-very-long-branch-name-to-keep-readable' });
    Object.assign(withWorktree.repositories[1], { worktreeCount: 1, worktrees: [{ id: 'opaque-worktree',
      name: 'web-review', branch: 'review', dirty: true, live: true, fileCount: 2, ahead: 1, behind: 0, available: true,
      groups: [
        { id: 'index', label: 'Staged', files: [{ id: 'worktree-staged', status: 'M', path: 'staged.txt', submodule: true,
          submoduleDetails: { state: 'dirty', head: { branch: 'feature/test' }, warn: { unpushed_commits: 2,
            pointer_unreachable: false, needs_merge_to_default: true } } }] },
        { id: 'working', label: 'Unstaged', files: [{ id: 'worktree-working', status: 'M', path: 'working.txt' }] }
      ] }] });
    window.postMessage(withWorktree, '*');
    await tick();
    let cleanRepo = items().find(item => item.getAttribute('aria-label').includes('web-app'));
    check('clean repository with worktrees is expandable', cleanRepo.getAttribute('aria-expanded') === 'false' && cleanRepo.textContent.includes('▸⎇1'));
    cleanRepo.querySelector('.row').click();
    cleanRepo = items().find(item => item.getAttribute('aria-label').includes('web-app'));
    let worktree = items().find(item => item.getAttribute('aria-label').includes('web-review, worktree'));
    check('worktree subtree icon uses native semantic color', getComputedStyle(worktree.querySelector('.subtree')).color ===
      document.documentElement.style.getPropertyValue('--gitpane-repo-list-worktree-subtree-icon'));
    check('workspace worktree status is visible', !!worktree && ['⎇', '*', 'review', '[2]', '↑1'].every(value => worktree.textContent.includes(value)));
    check('worktree live session is independent and accessible', worktree.textContent.includes('◉') &&
      worktree.getAttribute('aria-label').includes('live session') && !cleanRepo.querySelector('.repo-row .live'));
    check('worktree status has accessible meaning', ['has changes', '2 files', '1 ahead'].every(value => worktree.getAttribute('aria-label').includes(value)));
    check('dirty worktree is collapsed and expandable', worktree.getAttribute('aria-expanded') === 'false');
    const layoutRoot = document.querySelector('[role="tree"]');
    const worktreeFrame = document.createElement('iframe');
    worktreeFrame.style.cssText = 'position:fixed;left:-9999px;width:210px;height:650px;border:0';
    document.body.appendChild(worktreeFrame);
    worktreeFrame.contentDocument.head.appendChild(document.querySelector('style').cloneNode(true));
    worktreeFrame.contentDocument.body.className = document.body.className;
    worktreeFrame.contentDocument.body.appendChild(layoutRoot.cloneNode(true));
    const layouts = [210, 300, 500, 900].map(width => ({ layout: layoutRoot, width }));
    layouts.push({ layout: worktreeFrame.contentDocument.querySelector('[role="tree"]'), width: 210 });
    for (const { layout, width } of layouts) {
      layout.style.width = width + 'px';
      const rows = [...layout.querySelectorAll('.repo-row, .worktree-row')];
      const branchLeft = rows[0].querySelector('.branch').getBoundingClientRect().left;
      for (const row of rows) {
        const name = row.querySelector('.name');
        const branch = row.querySelector('.branch');
        const nameBounds = name.getBoundingClientRect();
        const branchBounds = branch.getBoundingClientRect();
        check('repository and worktree branch columns align at ' + width + 'px',
          Math.abs(branchBounds.left - branchLeft) < 1);
        check('branch follows its repository or worktree name at ' + width + 'px',
          nameBounds.right <= branchBounds.left);
        const markerText = document.createRange();
        markerText.selectNodeContents(row.querySelector('.dirty'));
        check('status markers leave room before the name at ' + width + 'px',
          nameBounds.left - markerText.getBoundingClientRect().right >= 4);
        const nameText = document.createRange();
        nameText.selectNodeContents(name);
        const branchText = document.createRange();
        branchText.selectNodeContents(branch);
        check('name and branch text share a baseline at ' + width + 'px',
          Math.abs(nameText.getBoundingClientRect().bottom - branchText.getBoundingClientRect().bottom) < 1);
        check('branch stays close to its name at ' + width + 'px',
          branchBounds.left - nameText.getBoundingClientRect().right <= parseFloat(getComputedStyle(row).fontSize) * 8);
        check('repository rows fit the pane at ' + width + 'px',
          row.getBoundingClientRect().right <= layout.getBoundingClientRect().right + 1);
        const tail = row.querySelector('.tail').getBoundingClientRect();
        for (const indicator of row.querySelector('.tail').children) {
          const bounds = indicator.getBoundingClientRect();
          check('repository and worktree badges stay visible at ' + width + 'px',
            bounds.left >= tail.left - 1 && bounds.right <= tail.right + 1);
        }
      }
    }
    worktreeFrame.remove();
    layoutRoot.style.width = '';
    const worktreeGraph = worktree.querySelector('.graph-action');
    check('worktree graph is accessible', worktreeGraph.getAttribute('aria-label') === 'Open commit graph for web-review');
    worktreeGraph.click();
    check('worktree graph sends only opaque id', window.posted.some(message => message.type === 'openWorktreeGraph' &&
      message.id === 'opaque-worktree' && !('path' in message)) && worktree.getAttribute('aria-expanded') === 'false');
    const worktreeMenu = worktree.querySelector('.menu-action');
    check('worktree action has an accessible name', worktreeMenu.getAttribute('aria-label') === 'Actions for worktree web-review');
    worktreeMenu.click();
    check('worktree menu sends only opaque id', window.posted.some(message => message.type === 'menu' &&
      message.kind === 'worktree' && message.id === 'opaque-worktree' && !('path' in message)) && cleanRepo.getAttribute('aria-expanded') === 'true');
    worktree.querySelector('.row').click();
    worktree = items().find(item => item.getAttribute('aria-label').includes('web-review, worktree'));
    check('worktree expands staged and unstaged files', worktree.getAttribute('aria-expanded') === 'true' &&
      worktree.textContent.includes('staged.txt') && worktree.textContent.includes('working.txt'));
    const nestedFile = items().find(item => item.getAttribute('aria-label') === 'M working.txt');
    const submoduleFile = items().find(item => item.getAttribute('aria-label').startsWith('M staged.txt'));
    check('regular and submodule paths use their native semantic colors',
      getComputedStyle(nestedFile.querySelector('.file-path')).color === document.documentElement.style.getPropertyValue('--gitpane-file-list-regular-path') &&
      getComputedStyle(submoduleFile.querySelector('.file-path')).color === document.documentElement.style.getPropertyValue('--gitpane-file-list-submodule-path'));
    check('submodule diagnostics use native symbols, colors and accessible labels',
      submoduleFile.querySelector('.submodule-tag').textContent === '[sub: ~dirty @feature/test ↑2 ↛main]' &&
      ['dirty', 'branch feature/test', '2 unpushed commits', 'needs merge to default branch'].every(value => submoduleFile.getAttribute('aria-label').includes(value)) &&
      getComputedStyle(submoduleFile.querySelector('.submodule-branch')).color === document.documentElement.style.getPropertyValue('--gitpane-file-list-submodule-branch'));
    root.style.width = '210px';
    const diagnosticRow = submoduleFile.querySelector('.row').getBoundingClientRect();
    const diagnosticPath = submoduleFile.querySelector('.file-path').getBoundingClientRect();
    check('submodule path remains within the narrow row', diagnosticPath.width > 0 && diagnosticPath.right <= diagnosticRow.right + 1);
    root.style.width = '';
    nestedFile.querySelector('.row').click();
    check('clicking a worktree file opens its native change', window.posted.some(message =>
      message.type === 'openChange' && message.id === 'worktree-working' && !('path' in message)));
    for (const key of ['p', 'P', 'v', 'o', 'G', 'g', 'y', 'j', 'w']) {
      nestedFile.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
      const message = window.posted.at(-1);
      check('shortcut targets the selected worktree ' + key,
        message.type === 'shortcut' && message.id === 'opaque-worktree' && message.key === key &&
        (key !== 'y' || message.fileId === 'worktree-working'));
    }
    const beforeModified = window.posted.length;
    for (const modifiers of [{ ctrlKey: true }, { altKey: true }, { metaKey: true }, { repeat: true }]) {
      nestedFile.dispatchEvent(new KeyboardEvent('keydown', { key: 'p', bubbles: true, ...modifiers }));
    }
    check('modified and repeated shortcuts never launch actions', window.posted.length === beforeModified);
    for (const key of ['q', '=', '?']) nestedFile.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
    check('reserved native keys never invoke custom launchers', window.posted.length === beforeModified);
    worktree.querySelector('.row').click();
    worktree = items().find(item => item.getAttribute('aria-label').includes('web-review, worktree'));
    worktree.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
    worktree = items().find(item => item.getAttribute('aria-label').includes('web-review, worktree'));
    check('keyboard expands the selected worktree', worktree.getAttribute('aria-expanded') === 'true');
    worktree.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    worktree = items().find(item => item.getAttribute('aria-label').includes('web-review, worktree'));
    check('keyboard collapses the worktree', worktree.getAttribute('aria-expanded') === 'false');
    worktree.querySelector('.row').click();
    const diagnosticFile = withWorktree.repositories[1].worktrees[0].groups[0].files[0];
    for (const [info, expected] of [
      [{ state: 'uninitialized', head: { branch: 'ignored' }, warn: { unpushed_commits: 2, pointer_unreachable: true, needs_merge_to_default: true } }, '[sub: -uninit]'],
      [{ state: 'modified', head: { detached: true }, warn: { unpushed_commits: 2, pointer_unreachable: true, needs_merge_to_default: true } }, '[sub: +commit @detached ⚠unreach]'],
      [{ state: null, head: { branch: '<img src=x>' }, warn: { unpushed_commits: 0, pointer_unreachable: false, needs_merge_to_default: false } }, '[sub: @<img src=x>]'],
      [{ state: null, head: { branch: 'feature\u202ehidden\u2066x\u2069\nname' }, warn: { unpushed_commits: 0, pointer_unreachable: false, needs_merge_to_default: false } }, '[sub: @feature hidden x  name]']
    ]) {
      diagnosticFile.submoduleDetails = info;
      window.postMessage(withWorktree, '*');
      await tick();
      const item = items().find(node => node.getAttribute('aria-label').startsWith('M staged.txt'));
      check('submodule diagnostic precedence and text-safe branch', item.querySelector('.submodule-tag').textContent === expected && !item.querySelector('img'));
      check('submodule accessible description removes control characters', !/[\x00-\x1f\x7f\u202a-\u202e\u2066-\u2069]/.test(item.getAttribute('aria-label')));
    }
    worktree = items().find(item => item.getAttribute('aria-label').includes('web-review, worktree'));
    worktree.querySelector('.row').click();
    const unavailable = JSON.parse(JSON.stringify(withWorktree));
    unavailable.repositories[1].worktrees[0].available = false;
    window.postMessage(unavailable, '*');
    await tick();
    worktree = items().find(item => item.getAttribute('aria-label').includes('web-review, worktree'));
    check('unavailable worktree is reported without stale file or graph actions',
      worktree.getAttribute('aria-label').includes('repository unavailable') && !worktree.querySelector('.graph-action') &&
      !items().some(item => item.getAttribute('aria-label') === 'M working.txt'));
    const overview = { type: 'snapshot', errors: [], loading: false, expandWorktrees: true, repositories: [{
      id: 'overview-parent', name: 'overview-parent', branch: 'main', dirty: false, groups: [],
      worktreeStateKey: 'a'.repeat(64), worktreeCount: 1,
      changesPage: { state: 'idle', page: 0, previous: false, next: false },
      worktrees: [{ id: 'overview-child', name: 'overview-child', branch: 'topic', dirty: true,
        changeCountLowerBound: 1, available: true, groups: [],
        changesPage: { state: 'idle', page: 0, previous: false, next: false } }]
    }] };
    const beforeOverview = window.posted.length;
    window.postMessage(overview, '*');
    await tick();
    let overviewParent = items().find(item => item.getAttribute('aria-label').startsWith('overview-parent'));
    let overviewChild = items().find(item => item.getAttribute('aria-label').startsWith('overview-child, worktree'));
    check('configured default shows a dirty worktree under a clean parent', !!overviewChild &&
      overviewChild.getAttribute('aria-label').includes('has changes') && overviewParent.getAttribute('aria-expanded') === 'true');
    check('showing worktrees does not load file pages', !window.posted.slice(beforeOverview).some(message => message.type === 'loadChanges'));
    let overviewButton = overviewParent.querySelector('.worktree-action');
    check('worktree toggle is accessible', overviewButton.getAttribute('aria-expanded') === 'true' &&
      overviewButton.getAttribute('aria-label') === 'Collapse worktrees for overview-parent');
    overviewChild.querySelector('.row').click();
    check('expanding a worktree loads only that worktree', window.posted.slice(beforeOverview).some(message =>
      message.type === 'loadChanges' && message.id === 'overview-child') && !window.posted.slice(beforeOverview).some(message =>
      message.type === 'loadChanges' && message.id === 'overview-parent'));
    overviewParent = items().find(item => item.getAttribute('aria-label').startsWith('overview-parent'));
    overviewParent.querySelector('.worktree-action').click();
    check('hiding worktrees releases their loaded pages', !items().some(item => item.getAttribute('aria-label').startsWith('overview-child, worktree')) &&
      window.posted.slice(beforeOverview).some(message => message.type === 'releaseChanges' && message.id === 'overview-child'));
    overviewParent = items().find(item => item.getAttribute('aria-label').startsWith('overview-parent'));
    overviewParent.focus();
    overviewParent.dispatchEvent(new KeyboardEvent('keydown', { key: 'w', bubbles: true }));
    overviewChild = items().find(item => item.getAttribute('aria-label').startsWith('overview-child, worktree'));
    check('keyboard restores remembered worktree expansion', !!overviewChild && overviewChild.getAttribute('aria-expanded') === 'true' &&
      window.posted.slice(beforeOverview).filter(message => message.type === 'loadChanges' && message.id === 'overview-child').length === 2);
    overviewChild.focus();
    overviewChild.dispatchEvent(new KeyboardEvent('keydown', { key: 'w', bubbles: true }));
    check('worktree shortcut works from a worktree row and restores focus to its parent',
      !items().some(item => item.getAttribute('aria-label').startsWith('overview-child, worktree')) &&
      document.activeElement.getAttribute('aria-label').startsWith('overview-parent'));
    overviewParent = items().find(item => item.getAttribute('aria-label').startsWith('overview-parent'));
    overviewParent.dispatchEvent(new KeyboardEvent('keydown', { key: 'w', bubbles: true }));
    const changedId = structuredClone(overview);
    changedId.repositories[0].id = 'overview-parent-reopened';
    changedId.expandWorktrees = false;
    window.postMessage(changedId, '*');
    await tick();
    check('user preference survives provider IDs and a late config change',
      !!items().find(item => item.getAttribute('aria-label').startsWith('overview-child, worktree')));
    changedId.repositories[0].worktrees.push({ id: 'new-worktree', name: 'new-worktree', branch: 'new', dirty: false,
      available: true, groups: [], changesPage: { state: 'idle', page: 0, previous: false, next: false } });
    changedId.repositories[0].worktreeCount = 2;
    window.postMessage(changedId, '*');
    await tick();
    check('a newly discovered workspace worktree appears automatically', !!items().find(item =>
      item.getAttribute('aria-label').startsWith('new-worktree, worktree')));
    const disabled = structuredClone(overview);
    disabled.expandWorktrees = false;
    disabled.repositories[0].id = 'default-off-parent';
    disabled.repositories[0].worktreeStateKey = 'b'.repeat(64);
    disabled.repositories[0].worktrees[0].id = 'default-off-child';
    window.postMessage(disabled, '*');
    await tick();
    let disabledParent = items().find(item => item.getAttribute('aria-label').startsWith('overview-parent'));
    check('disabled native default starts with worktrees hidden', disabledParent.getAttribute('aria-expanded') === 'false' &&
      !items().some(item => item.getAttribute('aria-label').startsWith('overview-child, worktree')));
    disabledParent.querySelector('.worktree-action').click();
    check('user can expand worktrees when the native default is disabled', !!items().find(item =>
      item.getAttribute('aria-label').startsWith('overview-child, worktree')));
    disabledParent = items().find(item => item.getAttribute('aria-label').startsWith('overview-parent'));
    disabledParent.querySelector('.worktree-action').click();
    const created = structuredClone(disabled);
    created.revealWorktrees = [{ id: 'created-worktree', identity: 'b'.repeat(64) }];
    window.postMessage(created, '*');
    await tick();
    check('a created worktree is revealed and acknowledged', !!items().find(item =>
      item.getAttribute('aria-label').startsWith('overview-child, worktree')) &&
      window.posted.some(message => message.type === 'ackWorktreeReveal' && message.id === 'created-worktree') &&
      window.savedState.appliedWorktreeReveals.includes('created-worktree') &&
      window.ackStates.some(state => state.appliedWorktreeReveals.includes('created-worktree')));
    disabledParent = items().find(item => item.getAttribute('aria-label').startsWith('overview-parent'));
    disabledParent.querySelector('.worktree-action').click();
    window.postMessage(created, '*');
    await tick();
    check('a duplicate reveal does not override a later user collapse', !items().some(item =>
      item.getAttribute('aria-label').startsWith('overview-child, worktree')) &&
      window.posted.filter(message => message.type === 'ackWorktreeReveal' && message.id === 'created-worktree').length === 2);
    const mixed = structuredClone(disabled);
    mixed.repositories[0].id = 'mixed-parent';
    mixed.repositories[0].worktreeStateKey = 'c'.repeat(64);
    mixed.repositories[0].stashCount = 1;
    mixed.repositories[0].stashes = [{ id: 'mixed-stash', index: 0, message: 'saved' }];
    window.postMessage(mixed, '*');
    await tick();
    let mixedParent = items().find(item => item.getAttribute('aria-label').startsWith('overview-parent'));
    mixedParent.querySelector('.stash-action').click();
    mixedParent = items().find(item => item.getAttribute('aria-label').startsWith('overview-parent'));
    mixedParent.focus();
    mixedParent.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
    mixedParent = items().find(item => item.getAttribute('aria-label').startsWith('overview-parent'));
    mixedParent.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
    check('left closes stashes without reopening hidden worktrees', !items().some(item =>
      item.getAttribute('aria-label').startsWith('overview-child, worktree')) &&
      !items().some(item => item.getAttribute('aria-label').startsWith('stash@{0}')));
    mixedParent = items().find(item => item.getAttribute('aria-label').startsWith('overview-parent'));
    mixedParent.querySelector('.stash-action').click();
    const mixedStash = items().find(item => item.getAttribute('aria-label').startsWith('stash@{0}'));
    mixedStash.focus();
    mixedStash.dispatchEvent(new KeyboardEvent('keydown', { key: 'w', bubbles: true }));
    check('worktree shortcut resolves a stash row to its parent repository', !!items().find(item =>
      item.getAttribute('aria-label').startsWith('overview-child, worktree')));
    window.postMessage({ type: 'snapshot', repositories: [], errors: [], loading: false, expandWorktrees: true }, '*');
    await tick();
    check('removed workspace worktrees disappear', !items().some(item => item.getAttribute('aria-label').includes('worktree')));
    window.postMessage(snapshot, '*');
    await tick();
    repo = items().find(item => item.getAttribute('aria-label').includes('api-service'));
    repo.focus();
    repo.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
    repo = items().find(item => item.getAttribute('aria-label').includes('api-service'));
    check('keyboard expands repository', repo.getAttribute('aria-expanded') === 'true');
    check('persist expansion', window.savedState.expanded.includes('repo:api'));
    check('staged and unstaged visible', items().some(item => item.getAttribute('aria-label').startsWith('Staged')) && items().some(item => item.getAttribute('aria-label').startsWith('Unstaged')));
    check('unsafe path rendered as text', document.body.textContent.includes(snapshot.repositories[0].groups[0].files[1].path));
    check('unsafe path did not create markup', !document.querySelector('img') && !window.injected);
    repo.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
    check('arrow down selects group', document.activeElement.getAttribute('aria-label').startsWith('Unstaged'));
    document.activeElement.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
    check('arrow down selects file', document.activeElement.getAttribute('aria-label').includes('src/server.rs'));
    const selectedFile = document.activeElement;
    const fileMenu = selectedFile.querySelector('.menu-action');
    check('file actions have an accessible name', fileMenu.getAttribute('aria-label') === 'Actions for src/server.rs');
    const openCount = window.posted.filter(message => message.type === 'openChange').length;
    fileMenu.click();
    check('file actions do not open the diff',
      window.posted.some(message => message.type === 'menu' && message.kind === 'file' && message.id === 'file1') &&
      window.posted.filter(message => message.type === 'openChange').length === openCount);
    document.activeElement.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    check('enter opens selected diff', window.posted.some(message => message.type === 'openChange' && message.id === 'file1'));
    const unsafe = items().find(item => item.getAttribute('aria-label').includes('<img'));
    unsafe.querySelector('.row').click();
    check('click opens selected diff', window.posted.some(message => message.type === 'openChange' && message.id === 'unsafe'));
    const reordered = JSON.parse(JSON.stringify(snapshot));
    reordered.repositories.reverse();
    window.postMessage(reordered, '*');
    await tick();
    check('repository reorder preserves selection and expansion', document.activeElement.dataset.key === 'file:unsafe' &&
      items().find(item => item.getAttribute('aria-label').includes('api-service')).getAttribute('aria-expanded') === 'true');
    window.postMessage({ type: 'snapshot', repositories: [], errors: ['Bad <img>'], loading: false }, '*');
    await tick();
    check('error announced', document.querySelector('[role="alert"]').textContent === 'Bad <img>');
    document.querySelector('.retry').click();
    check('retry action', window.posted.some(message => message.type === 'refresh'));
    window.postMessage(snapshot, '*');
    await tick();
    repo = items().find(item => item.getAttribute('aria-label').includes('api-service'));
    check('expansion survives updates', repo.getAttribute('aria-expanded') === 'true');
    const paged = { type: 'snapshot', errors: [], loading: false, repositories: [{
      id: 'paged', name: 'paged-repo', branch: 'main', dirty: true, groups: [],
      changeCountLowerBound: 3,
      changesPage: { state: 'idle', page: 0, previous: false, next: false, count: 0 }
    }] };
    window.postMessage(paged, '*');
    await tick();
    let pagedRepo = items().find(item => item.getAttribute('aria-label').includes('paged-repo'));
    check('unloaded repository displays a lower bound without claiming a total', pagedRepo.getAttribute('aria-expanded') === 'false' &&
      pagedRepo.textContent.includes('[3+]') && pagedRepo.getAttribute('aria-label').includes('at least 3 changes'));
    const initialRequests = window.posted.filter(message => message.type === 'loadChanges' && message.id === 'paged').length;
    pagedRepo.querySelector('.row').click();
    check('expansion requests the selected owner using only its opaque id', window.posted.filter(message =>
      message.type === 'loadChanges' && message.id === 'paged').length === initialRequests + 1 &&
      !('path' in window.posted.at(-1)));
    window.postMessage(paged, '*');
    await tick();
    check('repeated idle snapshots do not duplicate pending loads', window.posted.filter(message =>
      message.type === 'loadChanges' && message.id === 'paged').length === initialRequests + 1);
    paged.repositories[0].changesPage.state = 'loading';
    window.postMessage(paged, '*');
    await tick();
    check('loading is announced', [...document.querySelectorAll('[role="status"]')].some(node => node.textContent === 'Loading changes…'));
    pagedRepo = items().find(item => item.getAttribute('aria-label').includes('paged-repo'));
    check('loading keeps the compact-status count as a lower bound', pagedRepo.textContent.includes('[3+]') &&
      pagedRepo.getAttribute('aria-label').includes('at least 3 changes'));
    paged.repositories[0].changesPage = { state: 'ready', page: 0, previous: false, next: true, count: 1 };
    paged.repositories[0].groups = [{ id: 'index', label: 'Staged', files: [{ id: 'page-a', status: 'A', path: 'a.txt' }] }];
    window.postMessage(paged, '*');
    await tick();
    pagedRepo = items().find(item => item.getAttribute('aria-label').includes('paged-repo'));
    check('partial count retains the compact-status lower bound', pagedRepo.textContent.includes('[3+]') && pagedRepo.getAttribute('aria-label').includes('at least 3 changes'));
    const complete = structuredClone(paged);
    complete.repositories[0].changesPage.next = false;
    window.postMessage(complete, '*');
    await tick();
    pagedRepo = items().find(item => item.getAttribute('aria-label').includes('paged-repo'));
    check('a complete file list reports an exact file count', pagedRepo.textContent.includes('[1]') &&
      pagedRepo.getAttribute('aria-label').includes('1 file') && !pagedRepo.getAttribute('aria-label').includes('at least'));
    const withDirectory = structuredClone(paged);
    withDirectory.repositories[0].groups.push({ id: 'untracked', label: 'Untracked', files: [
      { id: 'nested-directory', status: '?', path: 'nested-repo/', directory: true }
    ] });
    window.postMessage(withDirectory, '*');
    await tick();
    const directory = items().find(item => item.getAttribute('aria-label') === '? nested-repo/, untracked directory');
    check('untracked directory is visibly identified without a file menu',
      directory && directory.textContent.includes('directory') && !directory.querySelector('.menu-action'));
    const completeDirectory = structuredClone(withDirectory);
    completeDirectory.repositories[0].changesPage.next = false;
    window.postMessage(completeDirectory, '*');
    await tick();
    pagedRepo = items().find(item => item.getAttribute('aria-label').includes('paged-repo'));
    check('a complete list containing an embedded directory counts changes',
      pagedRepo.getAttribute('aria-label').includes('2 changes'));
    window.postMessage(withDirectory, '*');
    await tick();
    const currentDirectory = items().find(item => item.getAttribute('aria-label') === '? nested-repo/, untracked directory');
    const beforeDirectoryClick = window.posted.length;
    currentDirectory.querySelector('.row').click();
    currentDirectory.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    check('untracked directory never opens a diff or action', window.posted.length === beforeDirectoryClick);
    window.postMessage(paged, '*');
    await tick();
    let nextPage = items().find(item => item.getAttribute('aria-label') === 'Next changes for paged-repo');
    check('next page is a keyboard tree item', !!nextPage && nextPage.getAttribute('tabindex') === '-1');
    nextPage.querySelector('.row').click();
    check('next page requests only the owner and direction', window.posted.some(message => message.type === 'changesPage' &&
      message.id === 'paged' && message.direction === 'next' && !('cursor' in message) && !('path' in message)));
    nextPage = items().find(item => item.getAttribute('aria-label') === 'Next changes for paged-repo');
    const pageRequests = window.posted.filter(message => message.type === 'changesPage' && message.id === 'paged').length;
    nextPage.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    check('pending page request is disabled without losing focus', nextPage.getAttribute('aria-disabled') === 'true' &&
      document.activeElement === nextPage && window.posted.filter(message => message.type === 'changesPage' && message.id === 'paged').length === pageRequests);
    paged.repositories[0].changesPage.state = 'loading';
    window.postMessage(paged, '*');
    await tick();
    paged.repositories[0].changesPage = { state: 'ready', page: 1, previous: true, next: false, count: 1 };
    paged.repositories[0].groups = [{ id: 'working', label: 'Unstaged', files: [{ id: 'page-b', status: 'M', path: 'b.txt' }] }];
    window.postMessage(paged, '*');
    await tick();
    let previousPage = items().find(item => item.getAttribute('aria-label') === 'Previous changes for paged-repo');
    check('page transition keeps focus near navigation', document.activeElement === previousPage && !!items().find(item => item.getAttribute('aria-label') === 'M b.txt'));
    previousPage.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    check('keyboard returns to previous page', window.posted.some(message => message.type === 'changesPage' &&
      message.id === 'paged' && message.direction === 'previous'));
    paged.repositories[0].changesPage.state = 'loading';
    window.postMessage(paged, '*');
    await tick();
    paged.repositories[0].changesPage = { state: 'error', page: 1, previous: true, next: false, count: 1, error: 'Page <bad>' };
    window.postMessage(paged, '*');
    await tick();
    check('page error is announced as text', [...document.querySelectorAll('[role="alert"]')].some(node => node.textContent === 'Page <bad>') && !document.querySelector('bad'));
    let retryPage = items().find(item => item.getAttribute('aria-label') === 'Retry loading changes for paged-repo');
    retryPage.querySelector('.row').click();
    const retryRequests = window.posted.filter(message => message.type === 'loadChanges' && message.id === 'paged').length;
    retryPage = items().find(item => item.getAttribute('aria-label') === 'Retry loading changes for paged-repo');
    retryPage.querySelector('.row').click();
    check('retry is accessible and does not duplicate pending requests', retryPage.getAttribute('aria-disabled') === 'true' &&
      window.posted.filter(message => message.type === 'loadChanges' && message.id === 'paged').length === retryRequests);
    pagedRepo = items().find(item => item.getAttribute('aria-label').includes('paged-repo'));
    pagedRepo.querySelector('.row').click();
    check('collapsing releases the owner using only its opaque id', window.posted.some(message =>
      message.type === 'releaseChanges' && message.id === 'paged' && !('path' in message) && !('snapshot' in message)));
    paged.repositories[0].changesPage = { state: 'idle', page: 0, previous: false, next: false, count: 0 };
    paged.repositories[0].groups = [];
    window.postMessage(paged, '*');
    await tick();
    check('collapsed owner does not restart loading', window.posted.filter(message =>
      message.type === 'loadChanges' && message.id === 'paged').length === retryRequests);
    items().find(item => item.getAttribute('aria-label').includes('paged-repo')).querySelector('.row').click();
    check('reopening a released owner starts one new load', window.posted.filter(message =>
      message.type === 'loadChanges' && message.id === 'paged').length === retryRequests + 1);
    const pagedWorktree = { type: 'snapshot', errors: [], loading: false, repositories: [{
      id: 'page-parent', name: 'parent', branch: 'main', dirty: false, groups: [], worktreeCount: 1,
      worktrees: [{ id: 'page-child', name: 'child', branch: 'topic', dirty: true, available: true, groups: [],
        changesPage: { state: 'idle', page: 0, previous: false, next: false, count: 0 } }]
    }] };
    window.postMessage(pagedWorktree, '*');
    await tick();
    items().find(item => item.getAttribute('aria-label').includes('parent')).querySelector('.row').click();
    let pagedChild = items().find(item => item.getAttribute('aria-label').includes('child, worktree'));
    check('unloaded dirty worktree can expand', pagedChild.getAttribute('aria-expanded') === 'false' && pagedChild.textContent.includes('[?]'));
    pagedChild.querySelector('.row').click();
    check('worktree loads through its opaque owner id', window.posted.some(message => message.type === 'loadChanges' &&
      message.id === 'page-child' && !('path' in message)));
    pagedWorktree.repositories[0].worktrees[0].changesPage = { state: 'ready', page: 0, previous: false, next: true, count: 1 };
    pagedWorktree.repositories[0].worktrees[0].groups = [{ id: 'working', label: 'Unstaged',
      files: [{ id: 'child-page-file', status: '?', path: 'nested.txt' }] }];
    window.postMessage(pagedWorktree, '*');
    await tick();
    pagedChild = items().find(item => item.getAttribute('aria-label').includes('child, worktree'));
    check('worktree page reports a partial count', pagedChild.textContent.includes('[1+]') && pagedChild.getAttribute('aria-label').includes('at least 1 change'));
    items().find(item => item.getAttribute('aria-label') === 'Next changes for child').querySelector('.row').click();
    check('worktree paging uses its owner id', window.posted.some(message => message.type === 'changesPage' &&
      message.id === 'page-child' && message.direction === 'next'));
    items().find(item => item.getAttribute('aria-label').includes('parent')).querySelector('.row').click();
    check('collapsing a parent releases its expanded worktree', window.posted.some(message =>
      message.type === 'releaseChanges' && message.id === 'page-child'));
    pagedWorktree.repositories[0].worktrees[0].changesPage = { state: 'idle', page: 0, previous: false, next: false };
    pagedWorktree.repositories[0].worktrees[0].groups = [];
    window.postMessage(pagedWorktree, '*');
    await tick();
    const childLoads = window.posted.filter(message => message.type === 'loadChanges' && message.id === 'page-child').length;
    items().find(item => item.getAttribute('aria-label').includes('parent')).querySelector('.row').click();
    check('reopening a parent reloads its restored expanded worktree', window.posted.filter(message =>
      message.type === 'loadChanges' && message.id === 'page-child').length === childLoads + 1);
    const attention = { type: 'snapshot', errors: [], expandWorktrees: true,
      theme: { name: 'custom', values: { 'repo-list-dirty-submodule': 'rgb(160, 80, 240)' }, lanes: [], authors: [] },
      repositories: [{
      id: 'attention-parent', name: 'warning-parent', branch: 'main', dirty: false, hasUnpushedSubmodules: true,
      changeCountLowerBound: 0, groups: [], changesPage: { state: 'idle', page: 0, previous: false, next: false },
      worktreeCount: 1, worktrees: [{ id: 'attention-child', name: 'warning-child', branch: 'topic', dirty: false,
        available: true, hasUnpushedSubmodules: true, changeCountLowerBound: 0, groups: [],
        changesPage: { state: 'idle', page: 0, previous: false, next: false } }]
    }] };
    window.postMessage(attention, '*');
    await tick();
    let warningParent = items().find(item => item.getAttribute('aria-label').startsWith('warning-parent'));
    check('clean repository exposes its worktree without a changed file count',
      warningParent.getAttribute('aria-expanded') === 'true' && !warningParent.querySelector('.count'));
    warningParent.querySelector('.row').click();
    let warningChild = items().find(item => item.getAttribute('aria-label').startsWith('warning-child, worktree'));
    check('clean linked worktree exposes its submodule warning', warningChild.getAttribute('aria-expanded') === 'false' &&
      warningChild.getAttribute('aria-label').includes('submodules need attention') && !warningChild.querySelector('.count'));
    const attentionChild = attention.repositories[0].worktrees[0];
    for (const [dirty, unpushed] of [[true, false], [true, true], [false, false]]) {
      attentionChild.hasDirtySubmodules = dirty;
      attentionChild.hasUnpushedSubmodules = unpushed;
      attentionChild.live = dirty && unpushed;
      attentionChild.ahead = dirty && unpushed ? 1 : 0;
      attentionChild.behind = dirty && unpushed ? 1 : 0;
      window.postMessage(attention, '*');
      await tick();
      warningChild = items().find(item => item.getAttribute('aria-label').startsWith('warning-child, worktree'));
      const label = warningChild.getAttribute('aria-label');
      const dirtyBadge = warningChild.querySelector('.worktree-row .dirty-submodule');
      const unpushedBadge = warningChild.querySelector('.worktree-row .unpushed-submodule');
      check('worktree submodule indicators reflect status ' + dirty + '/' + unpushed,
        !!dirtyBadge === dirty && !!unpushedBadge === unpushed &&
        label.includes('dirty submodules') === dirty && label.includes('submodules need attention') === unpushed);
      if (dirtyBadge) check('worktree dirty submodule uses native semantic color', dirtyBadge.textContent === '◈' &&
        getComputedStyle(dirtyBadge).color === document.documentElement.style.getPropertyValue('--gitpane-repo-list-dirty-submodule'));
      if (dirty && unpushed) {
        root.style.width = '210px';
        const bounds = warningChild.querySelector('.tail').getBoundingClientRect();
        const rowBounds = warningChild.querySelector('.worktree-row').getBoundingClientRect();
        const rootBounds = root.getBoundingClientRect();
        check('both worktree submodule indicators remain visible in a narrow sidebar', [dirtyBadge, unpushedBadge].every(badge => {
          const rect = badge.getBoundingClientRect();
          return rect.left >= bounds.left - 1 && rect.right <= bounds.right + 1 &&
            rect.right <= rowBounds.right + 1 && rect.right <= rootBounds.right + 1;
        }));
        root.style.width = '';
      }
    }
    attentionChild.hasUnpushedSubmodules = true;
    window.postMessage(attention, '*');
    await tick();
    warningChild = items().find(item => item.getAttribute('aria-label').startsWith('warning-child, worktree'));
    warningChild.querySelector('.row').click();
    check('warning worktree requests its own changes', window.posted.some(message =>
      message.type === 'loadChanges' && message.id === 'attention-child'));
    for (const owner of [attention.repositories[0], attention.repositories[0].worktrees[0]]) {
      owner.changesPage = { state: 'ready', page: 0, previous: false, next: false };
      owner.groups = [{ id: 'attention', label: 'Submodules needing attention',
        files: [{ id: owner.id + '-module', status: '!', path: 'vendor/module' }] }];
    }
    window.postMessage(attention, '*');
    await tick();
    warningParent = items().find(item => item.getAttribute('aria-label').startsWith('warning-parent'));
    warningChild = items().find(item => item.getAttribute('aria-label').startsWith('warning-child, worktree'));
    check('warning entries do not become changed-file counts', !warningParent.querySelector('.repo-row .count') &&
      !warningChild.querySelector('.worktree-row .count'));
    const warningFile = items().find(item => item.getAttribute('aria-label').startsWith('Submodule needs attention: vendor/module'));
    check('warning has distinct group and symbol', warningFile && warningFile.textContent.includes('!') &&
      items().some(item => item.getAttribute('aria-label').startsWith('Submodules needing attention')));
    warningFile.querySelector('.row').click();
    check('clicking warning opens the submodule inspector', window.posted.some(message =>
      message.type === 'openChange' && message.id === 'attention-parent-module'));
    for (const owner of [attention.repositories[0], attention.repositories[0].worktrees[0]]) {
      owner.dirty = true;
      owner.changeCountLowerBound = 4;
      owner.changesPage.next = true;
    }
    window.postMessage(attention, '*');
    await tick();
    warningParent = items().find(item => item.getAttribute('aria-label').startsWith('warning-parent'));
    warningChild = items().find(item => item.getAttribute('aria-label').startsWith('warning-child, worktree'));
    check('warning-only pages preserve the changed-file lower bound for repositories and worktrees',
      warningParent.querySelector('.repo-row .count')?.textContent.includes('[4+]') &&
      warningChild.querySelector('.worktree-row .count')?.textContent.includes('[4+]'));
    window.postMessage(snapshot, '*');
    await tick();
    check('theme background applied', getComputedStyle(document.body).backgroundColor !== 'rgba(0, 0, 0, 0)');
    const background = getComputedStyle(document.body).backgroundColor;
    for (const selector of ['.dirty', '.branch.default', '.branch:not(.default)', '.ahead', '.behind', '.untracked']) {
      const node = document.querySelector(selector);
      check('fallback contrast ' + selector, !!node && contrast(getComputedStyle(node).color, background) >= 4.5);
    }
    check('visible tree has keyboard tab stop', items().filter(item => item.getAttribute('tabindex') === '0').length === 1);
    const searchable = { type: 'snapshot', errors: [], loading: false, repositories: [{ id: 'search-repo', name: 'search-repo',
      branch: 'main', dirty: true, groups: [{ id: 'working', label: 'Unstaged', files: [{ id: 'first-file', path: 'first.txt', status: 'M' }] }],
      changesPage: { state: 'ready', page: 0, previous: false, next: true }, worktrees: [] }] };
    window.postMessage(searchable, '*');
    await tick();
    let searchRepo = items().find(item => item.getAttribute('aria-label').includes('search-repo'));
    searchRepo.querySelector('.row').click();
    searchRepo = items().find(item => item.getAttribute('aria-label').includes('search-repo'));
    searchRepo.dispatchEvent(new KeyboardEvent('keydown', { key: '/', bubbles: true }));
    const searchInput = document.getElementById('file-search');
    check('slash focuses file search', document.activeElement === searchInput);
    searchInput.value = 'later';
    searchInput.dispatchEvent(new Event('input', { bubbles: true }));
    await new Promise(resolve => setTimeout(resolve, 130));
    check('search targets the selected repository', window.posted.some(message => message.type === 'searchChanges' &&
      message.id === 'search-repo' && message.query === 'later' && message.direction === 'first'));
    searchable.repositories[0].groups[0].files = [{ id: 'later-file', path: 'src/LATER.txt', status: 'M' }];
    searchable.repositories[0].changesPage = { state: 'ready', page: 1, previous: true, next: false,
      search: { query: 'later', phase: 'ready', count: 2, position: 0, path: 'src/LATER.txt', group: 'working' } };
    window.postMessage(searchable, '*');
    await tick();
    check('search selects and highlights a match on another page', items().some(item => item.getAttribute('aria-selected') === 'true' &&
      item.getAttribute('aria-label').includes('src/LATER.txt') && item.querySelector('.search-match')));
    searchInput.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    check('enter opens the matching file diff', window.posted.some(message => message.type === 'openChange' && message.id === 'later-file'));
    searchInput.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
    check('arrow requests the next match', window.posted.some(message => message.type === 'searchChanges' && message.direction === 'next'));
    searchable.repositories[0].changesPage.search = { query: 'later', phase: 'ready', count: 0, position: null, path: null, group: null };
    window.postMessage(searchable, '*');
    await tick();
    const opens = window.posted.filter(message => message.type === 'openChange').length;
    searchInput.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    check('no match cannot open a stale file', window.posted.filter(message => message.type === 'openChange').length === opens &&
      document.getElementById('search-status').textContent === 'No matches');
    searchInput.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    check('escape clears search', document.getElementById('search-box').hidden && window.posted.some(message =>
      message.type === 'searchChanges' && message.id === 'search-repo' && message.query === ''));
    searchable.repositories[0].worktrees = [{ id: 'search-child', name: 'linked', branch: 'topic', dirty: true, available: true,
      groups: [{ id: 'working', label: 'Unstaged', files: [{ id: 'child-file', path: 'child.txt', status: 'M' }] }],
      changesPage: { state: 'ready', page: 0, previous: false, next: false } }];
    searchable.repositories[0].worktreeCount = 1;
    searchable.expandWorktrees = true;
    window.postMessage(searchable, '*');
    await tick();
    let searchChild = items().find(item => item.getAttribute('aria-label').includes('linked, worktree'));
    searchChild.querySelector('.row').click();
    searchChild = items().find(item => item.getAttribute('aria-label').includes('linked, worktree'));
    searchChild.dispatchEvent(new KeyboardEvent('keydown', { key: '/', bubbles: true }));
    searchInput.value = 'child';
    searchInput.dispatchEvent(new Event('input', { bubbles: true }));
    await new Promise(resolve => setTimeout(resolve, 130));
    check('search targets the selected worktree', window.posted.some(message => message.type === 'searchChanges' &&
      message.id === 'search-child' && message.query === 'child'));
    searchInput.value = 'x'.repeat(513);
    searchInput.dispatchEvent(new Event('input', { bubbles: true }));
    window.postMessage(searchable, '*');
    await tick();
    check('long queries remain an error after refresh', document.getElementById('search-status').textContent === 'Search is too long.');
    document.body.dataset.result = btoa(JSON.stringify({ ok: true, checks, background: getComputedStyle(document.body).backgroundColor }));
  } catch (error) {
    document.body.dataset.result = btoa(JSON.stringify({ ok: false, checks, error: error.stack || String(error) }));
  }
})();`;

const restoreScenario = String.raw`
(async () => {
  const checks = [];
  const check = (name, value) => { if (!value) throw new Error(name); checks.push(name); };
  const tick = () => new Promise(resolve => setTimeout(resolve, 30));
  const fixture = { type: 'snapshot', errors: [], loading: false, repositories: [{
    id: 'restored', name: 'restored', branch: 'main', dirty: true, groups: [], worktreeCount: 1,
    changesPage: { state: 'idle', page: 0, previous: false, next: false, count: 0 },
    worktrees: [{ id: 'restored-child', name: 'child', branch: 'topic', dirty: true, available: true,
      groups: [], changesPage: { state: 'idle', page: 0, previous: false, next: false, count: 0 } }]
  }] };
  try {
    window.postMessage(fixture, '*');
    await tick();
    const items = () => [...document.querySelectorAll('[role="treeitem"]')];
    let repo = items().find(item => item.getAttribute('aria-label').includes('restored'));
    const requests = id => window.posted.filter(message => message.type === 'loadChanges' && message.id === id).length;
    check('pending config does not infer the worktree default', repo.getAttribute('aria-expanded') === 'true' &&
      !items().some(item => item.getAttribute('aria-label').includes('child, worktree')) && requests('restored') === 1 && requests('restored-child') === 0);
    fixture.expandWorktrees = true;
    window.postMessage(fixture, '*');
    await tick();
    repo = items().find(item => item.getAttribute('aria-label').includes('restored'));
    const child = items().find(item => item.getAttribute('aria-label').includes('child, worktree'));
    check('saved repository and worktree expansion is restored after config loads', repo.getAttribute('aria-expanded') === 'true' &&
      child.getAttribute('aria-expanded') === 'true');
    check('restored owners each load once', requests('restored') === 1 && requests('restored-child') === 1);
    window.postMessage(fixture, '*');
    await tick();
    check('repeated restore snapshot does not duplicate loads', requests('restored') === 1 && requests('restored-child') === 1);
    repo = items().find(item => item.getAttribute('aria-label').includes('restored'));
    repo.querySelector('.row').click();
    check('collapsing parent files preserves its visible worktree', window.posted.some(message =>
      message.type === 'releaseChanges' && message.id === 'restored') && !window.posted.some(message =>
      message.type === 'releaseChanges' && message.id === 'restored-child') &&
      items().some(item => item.getAttribute('aria-label').includes('child, worktree')));
    repo = items().find(item => item.getAttribute('aria-label').includes('restored'));
    repo.querySelector('.worktree-action').click();
    check('hiding worktrees releases the child page', window.posted.some(message =>
      message.type === 'releaseChanges' && message.id === 'restored-child'));
    window.postMessage(fixture, '*');
    await tick();
    check('collapsed restored owners stay unloaded', requests('restored') === 1 && requests('restored-child') === 1);
    const resent = structuredClone(fixture);
    resent.repositories[0].worktreeStateKey = 'd'.repeat(64);
    resent.revealWorktrees = [{ id: 'already-revealed', identity: 'd'.repeat(64) }];
    window.postMessage(resent, '*');
    await tick();
    check('a duplicate reveal after recreation preserves the saved collapse',
      !items().some(item => item.getAttribute('aria-label').includes('child, worktree')) &&
      window.posted.some(message => message.type === 'ackWorktreeReveal' && message.id === 'already-revealed'));
    document.body.dataset.result = btoa(JSON.stringify({ ok: true, checks, background: getComputedStyle(document.body).backgroundColor }));
  } catch (error) {
    document.body.dataset.result = btoa(JSON.stringify({ ok: false, checks, error: error.stack || String(error) }));
  }
})();`;

function documentFor(theme, restored = false) {
  const light = theme === 'light';
  const colors = light
    ? '--vscode-sideBar-background:#fff;--vscode-sideBar-foreground:#333;--vscode-list-inactiveSelectionBackground:#ddd;--vscode-focusBorder:#007acc;'
    : '--vscode-sideBar-background:#1e1e1e;--vscode-sideBar-foreground:#ddd;--vscode-list-inactiveSelectionBackground:#383838;--vscode-focusBorder:#007acc;';
  return `<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'nonce-gitpane-test'; script-src 'nonce-gitpane-test'"><style nonce="gitpane-test">:root {${colors}}\n${stylesheet}</style></head>
<body class="vscode-${theme}"><div id="messages" aria-live="polite"></div><div class="search-toolbar"><button id="start-search" type="button">⌕</button><div id="search-box" hidden><input id="file-search" type="search"><button id="search-prev" type="button" disabled>↑</button><button id="search-next" type="button" disabled>↓</button><div id="search-status" role="status"></div></div></div><div id="repositories" role="tree" aria-label="Gitpane repositories"></div>
<script nonce="gitpane-test">window.posted=[];window.ackStates=[];window.savedState=${restored ? JSON.stringify({ expanded: ['repo:restored', 'worktree:restored-child'], worktreeVisibility: [['d'.repeat(64), false]], appliedWorktreeReveals: ['already-revealed'] }) : '{}'};window.acquireVsCodeApi=()=>({postMessage:m=>{window.posted.push(m);if(m.type==='ackWorktreeReveal')window.ackStates.push(structuredClone(window.savedState));},getState:()=>window.savedState,setState:s=>window.savedState=s});</script>
<script nonce="gitpane-test">${themeClient}</script><script nonce="gitpane-test">${client}</script><script nonce="gitpane-test">${restored ? restoreScenario : scenario.replace('SNAPSHOT_BASE64', encodedSnapshot)}</script></body></html>`;
}

function run(theme, restored = false) {
  const label = `${theme}${restored ? '-restored' : ''}`;
  const fixture = path.join(temporary, `${label}.html`);
  const profile = path.join(temporary, `profile-${label}`);
  const screenshot = path.join(os.tmpdir(), `gitpane-webview-${theme}.png`);
  fs.writeFileSync(fixture, documentFor(theme, restored));
  const args = ['--headless', '--disable-gpu', '--disable-background-networking', '--no-first-run',
    '--disable-extensions', `--user-data-dir=${profile}`, '--window-size=500,650', '--virtual-time-budget=4000'];
  const dom = spawnSync(browser, [...args, '--dump-dom', pathToFileURL(fixture).href], { encoding: 'utf8', timeout: 30000 });
  if (dom.error || dom.status !== 0) throw new Error(`Chrome ${theme} test failed: ${dom.error?.stack || dom.stderr}`);
  const encoded = dom.stdout.match(/data-result="([^"]+)"/)?.[1];
  if (!encoded) throw new Error(`Chrome ${theme} did not report test results:\n${dom.stdout}\n${dom.stderr}`);
  const result = JSON.parse(Buffer.from(encoded, 'base64').toString('utf8'));
  if (!result.ok) throw new Error(`Chrome ${theme} behavior failed: ${result.error}`);
  if (!restored) {
    const image = spawnSync(browser, [...args, `--screenshot=${screenshot}`, pathToFileURL(fixture).href], { encoding: 'utf8', timeout: 30000 });
    if (image.error || image.status !== 0 || !fs.existsSync(screenshot)) throw new Error(`Chrome ${theme} screenshot failed: ${image.error?.stack || image.stderr}`);
  }
  console.log(`${label}: ${result.checks.length} checks passed; background ${result.background}${restored ? '' : `; screenshot ${screenshot}`}`);
  return result;
}

try {
  const dark = run('dark');
  const light = run('light');
  if (dark.background === light.background) throw new Error('Light and dark theme backgrounds should differ.');
  run('dark', true);
} catch (error) {
  console.error(error.stack || String(error));
  process.exitCode = 1;
} finally {
  fs.rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
