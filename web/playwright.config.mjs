import { defineConfig } from '@playwright/test';

const port = 8094;
const ci = Boolean(process.env.CI);
const softwareWebGpu = process.env.PLAYWRIGHT_SOFTWARE_WEBGPU === '1';

// GitHub's Linux runners do not expose a hardware WebGPU adapter. Run Chromium
// against its bundled SwiftShader Vulkan adapter there so iced can initialize
// and canvas screenshots exercise the same renderer as a browser with a GPU.
const softwareWebGpuLaunchOptions = {
  args: [
    '--enable-unsafe-webgpu',
    '--enable-features=Vulkan',
    '--use-angle=vulkan',
    '--use-vulkan=swiftshader',
    '--use-webgpu-adapter=swiftshader',
    '--disable-vulkan-surface',
  ],
};

export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.spec.mjs',
  fullyParallel: false,
  workers: 1,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  reporter: ci ? 'github' : 'list',
  globalSetup: './e2e/global-setup.mjs',
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    browserName: 'chromium',
    // Use Playwright's pinned full Chromium build. Its legacy headless shell
    // cannot initialize iced's WebGPU renderer on otherwise supported hosts.
    channel: process.env.PLAYWRIGHT_CHANNEL || 'chromium',
    // WebGPU presentation is unreliable in headless Chromium on GPU-less Linux
    // hosts. CI supplies a virtual display with xvfb-run instead.
    headless: !softwareWebGpu,
    launchOptions: softwareWebGpu ? softwareWebGpuLaunchOptions : undefined,
    ignoreHTTPSErrors: true,
    viewport: { width: 1100, height: 900 },
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
});
