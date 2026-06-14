<script lang="ts">
  // Add SSH Host dialog (issue #26, ADR 0014). One required field: the OpenSSH
  // target (`prod`, `user@example.com`). It offers Test Connection before Save;
  // the test runs the same non-interactive `ssh -o BatchMode=yes <target> hitch
  // daemon proxy` / Hello path the real attach (issue #27) will use, and surfaces
  // a classified, actionable result inline. Save does NOT require a passing test
  // (the user may be offline) — but empty/duplicate/invalid targets are rejected.
  //
  // Hitch stores ONLY the trimmed target string: OpenSSH config, ssh-agent,
  // hardware keys, ProxyJump, and known_hosts remain the source of truth. The
  // dialog reuses the shared Paper Terminal dialog vocabulary (.modal/.field/.btn)
  // so it is visually indistinguishable from Clone / Add project.
  import { Dialog } from "bits-ui";
  import { addSshHost, isDuplicateTarget, testSshHost, validateTarget } from "../sshHosts";
  import { addSshHostOpen } from "../overlays";
  import type { SshTestResult } from "../types";

  let target = $state("");
  // Forward the local ssh-agent on the proxy ssh so the remote daemon's git ops
  // sign through it (silly-ridge-27). Defaults on; persisted per-host on Save.
  let forwardAgent = $state(true);
  let testing = $state(false);
  let testResult = $state<SshTestResult | null>(null);
  let errMsg = $state<string | null>(null);

  function onOpenChange(next: boolean) {
    addSshHostOpen.set(next);
    if (next) {
      target = "";
      forwardAgent = true;
      testing = false;
      testResult = null;
      errMsg = null;
    }
  }

  // A fresh edit invalidates the last test result and any save error, so stale
  // green/red copy never lingers under a changed target.
  function onInput() {
    testResult = null;
    errMsg = null;
  }

  const trimmed = $derived(target.trim());
  const canTest = $derived(!testing && trimmed.length > 0);
  // Save is allowed for any non-empty target (offline-friendly); validity +
  // duplicate are enforced in `save()` and surfaced inline.
  const canSave = $derived(!testing && trimmed.length > 0);

  async function test() {
    if (!canTest) return;
    const valid = validateTarget(target);
    if (!valid.ok) {
      errMsg = valid.error;
      testResult = null;
      return;
    }
    testing = true;
    errMsg = null;
    testResult = null;
    try {
      testResult = await testSshHost(valid.target, forwardAgent);
    } catch (err) {
      // An unexpected IPC failure (the command itself errored) — show it as a
      // failed test rather than a thrown dialog error.
      testResult = {
        ok: false,
        category: "network",
        message: err instanceof Error ? err.message : String(err),
      };
    } finally {
      testing = false;
    }
  }

  function save() {
    if (!canSave) return;
    const valid = validateTarget(target);
    if (!valid.ok) {
      errMsg = valid.error;
      return;
    }
    if (isDuplicateTarget(valid.target)) {
      errMsg = `“${valid.target}” is already saved.`;
      return;
    }
    const result = addSshHost(valid.target, forwardAgent);
    if (!result.ok) {
      errMsg = result.error;
      return;
    }
    addSshHostOpen.set(false);
  }
</script>

<Dialog.Root open={$addSshHostOpen} {onOpenChange}>
  <Dialog.Portal>
    <Dialog.Overlay class="modal-back" />
    <Dialog.Content class="modal" aria-describedby={undefined}>
      <div class="m-head">
        <Dialog.Title>Add SSH Host</Dialog.Title>
        <div class="sub">Reach a remote Hitch daemon through an OpenSSH target.</div>
      </div>
      <div class="m-body">
        <label class="field">
          <span>SSH target</span>
          <!-- svelte-ignore a11y_autofocus -->
          <input
            class="base"
            bind:value={target}
            oninput={onInput}
            placeholder="user@example.com or host-alias"
            autofocus
            onkeydown={(e) => e.key === "Enter" && void save()}
          />
        </label>
        <p class="help">
          Hitch uses your OpenSSH config and ssh-agent. It stores only this target —
          no keys, passphrases, ports, or usernames.
        </p>

        <label class="check">
          <input type="checkbox" bind:checked={forwardAgent} />
          <span>
            <span class="check-title">Forward SSH agent</span>
            <span class="check-sub"
              >Lets the remote daemon sign git push/pull/fetch through your local
              ssh-agent — no prompt on the remote.</span
            >
          </span>
        </label>

        {#if testResult}
          {#if testResult.ok}
            <div class="test-box ok">
              <span class="dot" aria-hidden="true"></span>
              <div>{testResult.message}</div>
            </div>
          {:else}
            <div class="test-box bad">
              <span class="dot" aria-hidden="true"></span>
              <div>
                <div>{testResult.message}</div>
                {#if testResult.detail}<pre class="detail">{testResult.detail}</pre>{/if}
              </div>
            </div>
          {/if}
        {/if}

        {#if errMsg}<p class="m-error">{errMsg}</p>{/if}
      </div>
      <div class="m-foot">
        <Dialog.Close class="btn">Cancel</Dialog.Close>
        <button class="btn" disabled={!canTest} onclick={() => void test()}>
          {testing ? "Testing…" : "Test Connection"}
        </button>
        <button class="btn primary" disabled={!canSave} onclick={() => void save()}>Save</button>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  /* Classified test-result box, mirroring RemoveWorktreeDialog's .warn-box
     vocabulary: a tinted hairline panel with a leading status dot. Success reads
     in the ok-green status hue; a classified failure reads in the oxide need hue.
     The dot carries color redundantly with the copy so it survives grayscale. */
  /* Forward-agent toggle: a hairline checkbox row matching the dialog's quiet
     field vocabulary. Title reads at the field weight; the sub-line is muted. */
  .check {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    margin-top: 10px;
    cursor: pointer;
  }
  .check input {
    margin-top: 2px;
    flex: 0 0 auto;
  }
  .check span {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .check-title {
    font-size: var(--r0);
    color: var(--ink-1);
  }
  .check-sub {
    font-size: 0.625rem;
    color: var(--ink-2);
    line-height: 1.4;
  }

  .test-box {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 8px 10px;
    border: 1px solid;
    border-radius: 0;
    font-size: var(--r0);
    line-height: 1.5;
  }
  .test-box .dot {
    width: 7px;
    height: 7px;
    flex: 0 0 7px;
    margin-top: 4px;
    border-radius: 50%;
  }
  .test-box.ok {
    color: var(--ink-1);
    background: var(--st-ok-glow);
    border-color: var(--st-ok);
  }
  .test-box.ok .dot {
    background: var(--st-ok);
  }
  .test-box.bad {
    color: var(--st-need);
    background: var(--st-need-wash);
    border-color: var(--st-need-line);
  }
  .test-box.bad .dot {
    background: var(--st-need);
  }
  .detail {
    margin: 6px 0 0;
    padding: 6px 8px;
    background: var(--paper-3);
    border: 1px solid var(--line);
    font-family: var(--mono);
    font-size: 0.625rem;
    color: var(--ink-2);
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 7.5em;
    overflow: auto;
  }
</style>
