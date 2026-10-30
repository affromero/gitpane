'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const targets = {
  'darwin-arm64': { platform: 'darwin', arch: 'arm64', rust: 'aarch64-apple-darwin' },
  'darwin-x64': { platform: 'darwin', arch: 'x64', rust: 'x86_64-apple-darwin' },
  'linux-arm64': { platform: 'linux', arch: 'arm64', rust: 'aarch64-unknown-linux-gnu' },
  'linux-x64': { platform: 'linux', arch: 'x64', rust: 'x86_64-unknown-linux-gnu' },
  'win32-x64': { platform: 'win32', arch: 'x64', rust: 'x86_64-pc-windows-msvc' }
};

function command(program, args, options = {}) {
  const result = spawnSync(program, args, options);
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const detail = result.stderr?.toString().trim();
    throw new Error(`${program} failed with exit code ${result.status}${detail ? `: ${detail}` : ''}`);
  }
  return result.stdout;
}

function main(args) {
  if (args.length !== 4 || args[0] !== '--target' || args[2] !== '--out') {
    throw new Error('Usage: node scripts/package-vsix.js --target <platform> --out <path.vsix>');
  }

  const targetName = args[1];
  const target = targets[targetName];
  if (!Object.hasOwn(targets, targetName)) {
    throw new Error(`Unsupported target ${targetName}. Choose: ${Object.keys(targets).join(', ')}`);
  }
  if (target.platform !== process.platform || target.arch !== process.arch) {
    throw new Error(`Target ${targetName} must be built on the same OS and architecture; this host is ${process.platform}-${process.arch}.`);
  }
  if (target.platform === 'linux' && !process.report.getReport().header.glibcVersionRuntime) {
    throw new Error('Linux packages require a glibc host; Alpine/musl is not supported.');
  }
  if (Number(process.versions.node.split('.')[0]) < 22) {
    throw new Error('Packaging requires Node.js 22 or newer.');
  }

  const output = path.resolve(args[3]);
  if (fs.existsSync(output)) throw new Error(`Output already exists: ${output}`);
  if (!fs.existsSync(path.dirname(output))) throw new Error(`Output directory does not exist: ${path.dirname(output)}`);

  const extension = path.resolve(__dirname, '..');
  const repository = path.resolve(extension, '..');
  const metadata = command('cargo', ['metadata', '--locked', '--no-deps', '--format-version', '1'], {
    cwd: repository, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024
  });
  const targetDir = JSON.parse(metadata).target_directory;
  command('cargo', ['build', '--locked', '--release', '--target', target.rust], {
    cwd: repository, stdio: 'inherit'
  });

  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'gitpane-vsix-'));
  try {
    const staging = path.join(temp, 'extension');
    fs.mkdirSync(staging, { recursive: true });
    for (const name of ['package.json', 'README.md']) {
      fs.copyFileSync(path.join(extension, name), path.join(staging, name));
    }
    fs.cpSync(path.join(extension, 'src'), path.join(staging, 'src'), { recursive: true });
    fs.copyFileSync(path.join(repository, 'LICENSE'), path.join(staging, 'LICENSE'));
    fs.mkdirSync(path.join(staging, 'bin'));
    const executable = target.platform === 'win32' ? 'gitpane.exe' : 'gitpane';
    const bundled = path.join(staging, 'bin', executable);
    fs.copyFileSync(path.join(targetDir, target.rust, 'release', executable), bundled);
    if (target.platform !== 'win32') fs.chmodSync(bundled, 0o755);

    // Windows shell arguments are fixed or selected from the target allowlist.
    command(process.platform === 'win32' ? 'npx.cmd' : 'npx', [
      '--yes', '@vscode/vsce@4.1.0', 'package', '--no-dependencies',
      '--target', targetName, '--out', 'gitpane.vsix'
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
