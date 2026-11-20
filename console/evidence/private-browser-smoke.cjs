const fs = require("node:fs/promises");
const path = require("node:path");

class ConfigurationError extends Error {}

function required(name) {
  const value = process.env[name];
  if (!value?.trim()) throw new ConfigurationError(`${name} is required`);
  return value;
}

function inside(parent, child) {
  const relative = path.relative(parent, child);
  return !relative || (!path.isAbsolute(relative) && relative !== ".." && !relative.startsWith(`..${path.sep}`));
}

async function privateOutput() {
  const directory = path.resolve(required("OUT_DIR"));
  const checkout = await fs.realpath(path.join(__dirname, "../.."));
  const message = "OUT_DIR must be a private directory owned by the current user outside the checkout";
  if (inside(checkout, directory)) throw new ConfigurationError(message);
  try {
    await fs.mkdir(directory, { recursive: true, mode: 0o700 });
    const info = await fs.lstat(directory);
    if (!info.isDirectory() || info.uid !== process.getuid() || (info.mode & 0o077)
        || inside(checkout, await fs.realpath(directory))) {
      throw new Error();
    }
  } catch {
    throw new ConfigurationError(message);
  }
  return directory;
}

async function runPrivateSmoke(chromium, exercise, { tunnelUrl, agent = false }) {
  try {
    const key = required("DKEY");
    const agentId = agent ? required("DEEP_AGENT_ID") : undefined;
    let url;
    try {
      url = new URL(process.env.BASE_URL || tunnelUrl);
      if (!["http:", "https:"].includes(url.protocol) || url.username || url.password) throw new Error();
    } catch {
      throw new ConfigurationError("BASE_URL must be an HTTP or HTTPS URL");
    }
    const output = await privateOutput();
    const browser = await chromium.launch({
      headless: true,
      executablePath: process.env.CHROME_PATH || undefined,
    });
    try {
      await exercise({ browser, key, output, url, agentId });
    } finally {
      await browser.close();
    }
  } catch (error) {
    console.error(error instanceof ConfigurationError
      ? error.message
      : "Browser smoke failed. Check the endpoint, operator access, and expected view.");
    process.exitCode = 1;
  }
}

module.exports = { runPrivateSmoke };
