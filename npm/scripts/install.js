const fs = require("fs");
const https = require("https");
const os = require("os");
const path = require("path");
const { execFileSync } = require("child_process");

const repo = "Aero123421/whisperccpcli";
const version = process.env.WHISPERCLI_VERSION || "latest";
const vendorDir = path.join(__dirname, "..", "vendor");

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
    const request = https.get(url, (response) => {
      if ([301, 302, 303, 307, 308].includes(response.statusCode)) {
        download(response.headers.location, outFile).then(resolve, reject);
        return;
      }

      if (response.statusCode !== 200) {
        reject(new Error(`Download failed: ${response.statusCode} ${response.statusMessage}`));
        return;
      }

      const file = fs.createWriteStream(outFile);
      response.pipe(file);
      file.on("finish", () => file.close(resolve));
      file.on("error", reject);
    });

    request.on("error", reject);
  });
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
  const url = downloadUrl(asset);

  fs.rmSync(vendorDir, { recursive: true, force: true });
  fs.mkdirSync(vendorDir, { recursive: true });
  ensureUserDirs();

  console.log(`Downloading whisperCLI from ${url}`);
  await download(url, archive);
  extract(archive, vendorDir);
  fs.rmSync(archive, { force: true });
}

main().catch((error) => {
  console.error(`Failed to install whisperCLI: ${error.message}`);
  process.exit(1);
});
