import { startFixture } from './serve.mjs';

export default async function globalSetup() {
  return startFixture();
}
