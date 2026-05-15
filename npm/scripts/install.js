const fs = require("fs");
const https = require("https");
const crypto = require("crypto");
const os = require("os");
const path = require("path");
const { execFileSync } = require("child_process");

const repo = "Aero123421/whisperccpcli";
const packageVersion = require("../package.json").version;
const version = normalizeVersion(process.env.WHISPERCLI_VERSION || `v${packageVersion}`);
const defaultVendorDir = path.join(__dirname, "..", "vendor");
const vendorDir = path.resolve(process.env.WHISPERCLI_INSTALL_DIR || defaultVendorDir);
const skipDownload = isTruthy(process.env.WHISPERCLI_SKIP_DOWNLOAD);

function isTruthy(value) {
  return /^(1|true|yes|y)$/i.test(String(value || "").trim());
}

function normalizeVersion(rawVersion) {
  if (rawVersion === "latest") {
    return rawVersion;
  }

  return rawVersion.startsWith("v") ? rawVersion : `v${rawVersion}`;
}

function assetName() {
  const platform = process.platform;
  const arch = process.arch;

  if (platform === "win32" && arch === "x64") return "whispercli-windows-x64.zip";
  if (platform === "linux" && arch === "x64") return "whispercli-linux-x64.tar.gz";
  if (platform === "darwin" && arch === "x64") return "whispercli-macos-x64.tar.gz";
  if (platform === "darwin" && arch === "arm64") return "whispercli-macos-arm64.tar.gz";

  throw new Error(`Unsupported platform: ${platform} ${arch}`);
}

function downloadUrl(asset) {
  if (version === "latest") {
    return `https://github.com/${repo}/releases/latest/download/${asset}`;
  }

  return `https://github.com/${repo}/releases/download/${version}/${asset}`;
}

function download(url, outFile) {
  return new Promise((resolve, reject) => {
    https.get(url, (response) => {
      if ([301, 302, 303, 307, 308].includes(response.statusCode)) {
        return download(response.headers.location, outFile).then(resolve, reject);
      }

      if (response.statusCode !== 200) {
        reject(new Error(`Download failed: ${response.statusCode} ${response.statusMessage}`));
        return;
      }

      const file = fs.createWriteStream(outFile);
      response.pipe(file);
      file.on("finish", () => file.close(resolve));
      file.on("error", reject);
    }).on("error", reject);
  });
}

function downloadText(url) {
  return new Promise((resolve, reject) => {
    https.get(url, (response) => {
      if ([301, 302, 303, 307, 308].includes(response.statusCode)) {
        return downloadText(response.headers.location).then(resolve, reject);
      }

      if (response.statusCode !== 200) {
        reject(new Error(`Download failed: ${response.statusCode} ${response.statusMessage}`));
        return;
      }

      const chunks = [];
      response.on("data", (chunk) => chunks.push(chunk));
      response.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
      response.on("error", reject);
    }).on("error", reject);
  });
}

function checksumUrl() {
  if (version === "latest") {
    return `https://github.com/${repo}/releases/latest/download/checksums.txt`;
  }

  return `https://github.com/${repo}/releases/download/${version}/checksums.txt`;
}

function getExpectedChecksum(asset, text) {
  for (const line of text.split(/\r?\n/)) {
    const columns = line.trim().split(/\s+/);
    if (columns.length !== 2) {
      continue;
    }

    const [hash, name] = columns;
    if (name === asset || name === `*${asset}`) {
      return hash;
    }
  }

  return null;
}

async function verifyChecksum(asset, archive) {
  let checksumText;
  try {
    checksumText = await downloadText(checksumUrl());
  } catch {
    console.log("SHA256 checksums.txt is unavailable; skipping checksum verification.");
    return;
  }

  const expected = getExpectedChecksum(asset, checksumText);
  if (!expected) {
    console.log("checksum entry was not found for the selected asset; skipping checksum verification.");
    return;
  }

  const hash = crypto.createHash("sha256");
  await new Promise((resolve, reject) => {
    const source = fs.createReadStream(archive);
    source.on("data", (chunk) => hash.update(chunk));
    source.on("error", reject);
    source.on("end", resolve);
  });
  const actual = hash.digest("hex");

  if (actual !== expected) {
    throw new Error(`SHA256 mismatch for ${asset}: expected ${expected}, got ${actual}`);
  }

  console.log(`SHA256 OK: ${asset}`);
}

function getBinaryPath(baseDir) {
  return path.join(baseDir, process.platform === "win32" ? "whispercli.exe" : "whispercli");
}

function binaryExists(baseDir) {
  return fs.existsSync(getBinaryPath(baseDir));
}

function extract(archive, destination) {
  fs.mkdirSync(destination, { recursive: true });

  if (archive.endsWith(".zip")) {
    const psQuote = (value) => `'${value.replace(/'/g, "''")}'`;
    execFileSync("powershell", [
      "-NoProfile",
      "-ExecutionPolicy",
      "Bypass",
      "-Command",
      `Expand-Archive -LiteralPath ${psQuote(archive)} -DestinationPath ${psQuote(destination)} -Force`,
    ], { stdio: "inherit" });
    return;
  }

  execFileSync("tar", ["-xzf", archive, "-C", destination], { stdio: "inherit" });
  fs.chmodSync(path.join(destination, "whispercli"), 0o755);
}

function ensureUserDirs() {
  const root = path.join(os.homedir(), ".whispercli");
  for (const dir of ["bin", "models", "transcripts", "logs"]) {
    fs.mkdirSync(path.join(root, dir), { recursive: true });
  }
}

async function main() {
  const asset = assetName();
  const archive = path.join(os.tmpdir(), `${Date.now()}-${asset}`);

  try {
    ensureUserDirs();
    if (skipDownload) {
      if (!binaryExists(vendorDir) && !binaryExists(defaultVendorDir)) {
        throw new Error("WHISPERCLI_SKIP_DOWNLOAD is set, but no installed binary was found.");
      }

      return;
    }

    const url = downloadUrl(asset);

    fs.rmSync(vendorDir, { recursive: true, force: true });
    fs.rmSync(defaultVendorDir, { recursive: true, force: true });
    fs.mkdirSync(vendorDir, { recursive: true });

    console.log(`Downloading whisperCLI from ${url}`);
    await download(url, archive);
    await verifyChecksum(asset, archive);
    extract(archive, vendorDir);

    if (vendorDir !== defaultVendorDir) {
      const sourceBinary = getBinaryPath(vendorDir);
      const packageBinary = getBinaryPath(defaultVendorDir);
      if (!fs.existsSync(sourceBinary)) {
        throw new Error("Downloaded archive does not contain whispercli binary.");
      }

      fs.mkdirSync(defaultVendorDir, { recursive: true });
      fs.copyFileSync(sourceBinary, packageBinary);
      if (process.platform !== "win32") {
        fs.chmodSync(packageBinary, 0o755);
      }
    }
  } finally {
    fs.rmSync(archive, { force: true });
  }
}

main().catch((error) => {
  console.error(`Failed to install whisperCLI: ${error.message}`);
  process.exit(1);
});
