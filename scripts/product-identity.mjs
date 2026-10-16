// User-facing identity is separate from persistent compatibility identifiers.
export function assertProductIdentity(packageJson, config, cargoToml) {
  const packageSection = cargoToml.match(/^\[package\]\s*\r?\n([\s\S]*?)(?=^\[|$)/m)?.[1] ?? "";
  const cargoName = packageSection.match(/^name\s*=\s*"([^"]+)"/m)?.[1];
  const librarySection = cargoToml.match(/^\[lib\]\s*\r?\n([\s\S]*?)(?=^\[|$)/m)?.[1] ?? "";
  const libraryName = librarySection.match(/^name\s*=\s*"([^"]+)"/m)?.[1];
  const expected = [
    ["npm name", packageJson.name, "teacell-ai-media-manager"],
    ["Cargo package name", cargoName, "teacell_ai_media_manager"],
    ["Cargo library crate name", libraryName, "teacell_ai_media_manager_lib"],
    ["productName", config.productName, "TeaCell AI Media Manager"],
    ["publisher", config.bundle?.publisher, "TeaCell"],
    ["homepage", config.bundle?.homepage, "https://github.com/SSRYLJRSS/TeaCell_AiMedias"],
    ["window title", config.app?.windows?.find((window) => window.label === "main")?.title, "茶馆"],
  ];
  for (const [field, actual, value] of expected) {
    if (actual !== value) throw new Error(`${field} 必须为 ${value}`);
  }
  // No alternative filename may silently bring back an old executable name.
  if (config.mainBinaryName && config.mainBinaryName !== cargoName) {
    throw new Error(`mainBinaryName 必须与 Cargo package name 一致：${cargoName}`);
  }
  // Preserve installed libraries, credentials and the Windows upgrade family.
  if (config.identifier !== "com.bagertea.aimdeias"
    || config.bundle?.windows?.wix?.upgradeCode !== "30bc1910-3467-5659-bcff-284af99c9896") {
    throw new Error("品牌更名不得改变应用数据标识或 MSI upgradeCode；需要单独的数据迁移方案");
  }
}
