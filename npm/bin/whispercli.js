#!/usr/bin/env node

const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");

const exe = process.platform === "win32" ? "whispercli.exe" : "whispercli";
const binary = path.join(__dirname, "..", "vendor", exe);

if (!fs.existsSync(binary)) {
  console.error("whisperCLI binary is missing. Try reinstalling the npm package.");
  process.exit(1);
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });

if (result.error) {
  console.error(result.error.message);
  process.exit(1);
}

process.exit(result.status ?? 0);
