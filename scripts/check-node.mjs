const [major, minor] = process.versions.node.split(".").map(Number);

const supported =
  (major === 20 && minor >= 19) ||
  (major === 22 && minor >= 12) ||
  major > 22;

if (!supported) {
  console.error(
    `Unsupported Node.js ${process.versions.node}. Use Node 20.19+ or 22.12+.`,
  );
  process.exit(1);
}
