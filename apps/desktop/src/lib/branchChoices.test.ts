import { describe, expect, it } from "vitest";
import { localBranchNameForRemote, remoteBranchChoices } from "./branchChoices";

const local = (name: string) => ({ name, is_remote: false });
const remote = (name: string) => ({ name, is_remote: true });

describe("branch choice helpers", () => {
  it("derives local branch names from remote refs without the remote prefix", () => {
    expect(localBranchNameForRemote("origin/feature/demo")).toBe("feature/demo");
    expect(localBranchNameForRemote("origin/main")).toBe("main");
  });

  it("omits symbolic remote HEAD refs and remotes with existing local branches", () => {
    expect(
      remoteBranchChoices([
        local("main"),
        local("feature/existing"),
        remote("origin/HEAD"),
        remote("upstream/HEAD"),
        remote("origin/main"),
        remote("origin/feature/existing"),
        remote("origin/feature/new"),
      ]).map((b) => b.name),
    ).toEqual(["origin/feature/new"]);
  });
});
