import type { BranchSummary } from "./types";

export function localBranchNameForRemote(name: string): string {
  const slash = name.indexOf("/");
  return slash === -1 ? name : name.slice(slash + 1);
}

export function isSymbolicRemoteHead(name: string): boolean {
  return name === "HEAD" || name.endsWith("/HEAD");
}

export function remoteBranchChoices(branches: BranchSummary[]): BranchSummary[] {
  const localBranchNames = new Set(
    branches.filter((branch) => !branch.is_remote).map((branch) => branch.name),
  );
  return branches.filter(
    (branch) =>
      branch.is_remote &&
      !isSymbolicRemoteHead(branch.name) &&
      !localBranchNames.has(localBranchNameForRemote(branch.name)),
  );
}
