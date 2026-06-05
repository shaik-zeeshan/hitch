export type DesktopPlatform = "macos" | "windows" | "linux" | "other";
type NavigatorWithUserAgentData = Navigator & {
  userAgentData?: {
    platform?: string;
  };
};


export function desktopPlatformFromPlatform(platform: string | undefined): DesktopPlatform {
  const lower = (platform ?? "").toLowerCase();
  if (lower.includes("mac")) return "macos";
  if (lower.includes("win")) return "windows";
  if (lower.includes("linux")) return "linux";
  return "other";
}

export function revealItemLabel(platform: DesktopPlatform): string {
  switch (platform) {
    case "macos":
      return "Reveal in Finder";
    case "windows":
      return "Show in Explorer";
    case "linux":
    case "other":
      return "Show in file manager";
  }
}

export function currentDesktopPlatform(): DesktopPlatform {
  const nav = globalThis.navigator as NavigatorWithUserAgentData | undefined;
  return desktopPlatformFromPlatform(nav?.userAgentData?.platform ?? nav?.platform);
}

export function shortcutModifierLabel(platform: DesktopPlatform): string {
  return platform === "macos" ? "⌘" : "Ctrl";
}

export function shortcutLabel(platform: DesktopPlatform, key: string): string {
  return platform === "macos"
    ? `${shortcutModifierLabel(platform)}${key}`
    : `${shortcutModifierLabel(platform)}+${key}`;
}

export function shellSessionShortcutLabel(platform: DesktopPlatform): string {
  return shortcutLabel(platform, "T");
}

export function isShortcutModifier(
  event: { metaKey: boolean; ctrlKey: boolean },
  platform: DesktopPlatform,
): boolean {
  return platform === "macos" ? event.metaKey : event.ctrlKey;
}
