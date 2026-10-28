'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

function command(program, args, options = {}) {
  const result = spawnSync(program, args, options);
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${program} failed with exit code ${result.status}`);
  return result.stdout;
}

function main(args) {
  if (args.length !== 2 || args[0] !== '--out') {
    throw new Error('Usage: node scripts/package-vsix.js --out <path.vsix>');
  }

  const output = path.resolve(args[1]);
  if (fs.existsSync(output)) throw new Error(`Output already exists: ${output}`);
  if (!fs.existsSync(path.dirname(output))) throw new Error(`Output directory does not exist: ${path.dirname(output)}`);

  const extension = path.resolve(__dirname, '..');
  const repository = path.resolve(extension, '..');
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-vsix-'));
  try {
    const staging = path.join(temp, 'extension');
    fs.mkdirSync(staging, { recursive: true });
    for (const name of ['package.json', 'README.md']) {
      fs.copyFileSync(path.join(extension, name), path.join(staging, name));
    }
    fs.cpSync(path.join(extension, 'src'), path.join(staging, 'src'), { recursive: true });
    fs.copyFileSync(path.join(repository, 'LICENSE'), path.join(staging, 'LICENSE'));

    // All shell arguments on Windows are fixed strings.
    command(process.platform === 'win32' ? 'npx.cmd' : 'npx', [
      '--yes', '@vscode/vsce@4.1.0', 'package', '--no-dependencies',
      '--out', 'gitpane.vsix'
    ], { cwd: staging, stdio: 'inherit', shell: process.platform === 'win32' });
    fs.copyFileSync(path.join(staging, 'gitpane.vsix'), output, fs.constants.COPYFILE_EXCL);
    process.stdout.write(`VSIX written to ${output}\n`);
  } finally {
    fs.rmSync(temp, { recursive: true, force: true });
  }
}

try {
  main(process.argv.slice(2));
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
}
