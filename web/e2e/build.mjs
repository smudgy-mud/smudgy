import { spawn } from 'node:child_process';

// Trunk 0.21 rejects NO_COLOR=1 even though that is the conventional setting
// in CI and agent terminals. An explicit boolean keeps its parser portable.
function trunk(args) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.platform === 'win32' ? 'trunk.exe' : 'trunk', args, {
      stdio: 'inherit', env: { ...process.env, NO_COLOR: 'false' },
    });
    child.on('error', reject);
    child.on('exit', (code) => {
      if (code === 0) resolve();
      else reject(new Error(`trunk ${args[0]} exited with code ${code}`));
    });
  });
}

// Trunk can retain a prior snippet's SRI digest across incremental builds
// while copying newer JS bytes. Clean only the disposable E2E dist first.
await trunk(['clean', '--dist', 'e2e-dist']);
await trunk(['build', 'index.html', '--release', '--dist', 'e2e-dist']);
