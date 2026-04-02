const fs = require("fs");
const path = require("path");

const root = path.join(process.cwd(), "tmp-fs-metadata-rename");
fs.rmSync(root, { recursive: true, force: true });
fs.mkdirSync(root, { recursive: true });
fs.mkdirSync(path.join(root, "sub"));
fs.writeFileSync(path.join(root, "file.txt"), "x".repeat(2048));

const entries = fs
  .readdirSync(root, { withFileTypes: true })
  .map((entry) => [entry.name, entry.isDirectory()])
  .sort((a, b) => a[0].localeCompare(b[0]));

const filePath = path.join(root, "file.txt");
const renamedPath = path.join(root, "renamed.txt");
const statSize = fs.statSync(filePath).size;
const beforeExists = fs.existsSync(filePath);

fs.renameSync(filePath, renamedPath);

const summary = {
  entries,
  statSize,
  beforeExists,
  afterOldExists: fs.existsSync(filePath),
  afterNewExists: fs.existsSync(renamedPath),
  renamedSize: fs.statSync(renamedPath).size,
};

console.log(JSON.stringify(summary));

fs.rmSync(root, { recursive: true, force: true });
