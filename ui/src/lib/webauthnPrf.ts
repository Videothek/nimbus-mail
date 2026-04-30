// WebAuthn PRF helper for the FIDO unlock feature (#164).
//
// We use the PRF extension — WebAuthn's standardised wrapping of
// CTAP2's hmac-secret — to derive a stable 32-byte key from a FIDO
// authenticator (USB hardware key, Touch ID, Windows Hello, ...).
// The OS handles the auth UX entirely; we hand the PRF output to
// the Rust side which uses it as an AES-256-GCM key to wrap /
// unwrap the SQLCipher master key.
//
// Browser support landscape (as of 2026):
// - Safari 17+ on macOS / iOS — supports PRF.
// - Edge / Chrome on Windows / macOS — supports PRF for hmac-secret.
// - WebKitGTK on Linux — depends on the libwebkit2gtk-4.1 build
//   shipped by the distro; users may need a recent enough version.
// All fall back to "PRF extension not supported" with a clean
// error if the engine doesn't expose it; the Settings UI surfaces
// that as a tooltip on the disabled button.

const RP_ID = 'nimbus-mail.local'
const RP_NAME = 'Nimbus Mail'

function bufToB64(buf: ArrayBuffer | Uint8Array): string {
  const bytes = buf instanceof Uint8Array ? buf : new Uint8Array(buf)
  let bin = ''
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i])
  return btoa(bin)
}

function b64ToBuf(s: string): Uint8Array {
  const bin = atob(s)
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}

/**
 * True if the current webview exposes the WebAuthn PRF extension.
 * Used by the Settings UI to gate the "Add hardware key" button —
 * older WebKitGTK builds don't ship PRF support.
 */
export async function isPrfSupported(): Promise<boolean> {
  // PublicKeyCredential is the entry point for WebAuthn; if it's
  // not present the engine doesn't know about the spec at all.
  if (typeof PublicKeyCredential === 'undefined') return false
  if (typeof PublicKeyCredential.getClientCapabilities === 'function') {
    try {
      const caps = await PublicKeyCredential.getClientCapabilities()
      const r = caps as Record<string, boolean>
      if (typeof r.extensionPrf === 'boolean') return r.extensionPrf
    } catch {
      /* fall through to feature-test */
    }
  }
  // Best-effort feature detect — if we got this far, assume the
  // extension *might* work and let the caller surface a real
  // error from the create() call if it doesn't.
  return true
}

export interface EnrolledCredential {
  /** Base64-encoded credential id from the authenticator. */
  credentialIdB64: string
  /** Base64-encoded PRF output (32 bytes). */
  prfOutputB64: string
  /** Base64-encoded salt the credential was registered with. */
  saltB64: string
}

/**
 * Run WebAuthn `credentials.create` with the PRF extension enabled
 * and the supplied salt as the eval input.  Returns the credential
 * id plus the matching PRF output, both base64-encoded for the
 * Rust IPC.
 *
 * The OS shows its own auth sheet — Touch ID prompt, Windows Hello
 * sign-in, "Tap your security key", whatever's appropriate.  We
 * never see the biometric or the per-credential secret; we only
 * receive the deterministic PRF output that's reproducible by
 * future `credentials.get` calls with the same salt.
 */
export async function enrollFidoCredential(
  saltB64: string,
  userHandle: string,
  label: string,
): Promise<EnrolledCredential> {
  const salt = b64ToBuf(saltB64)
  // Random per-call challenge.  PRF doesn't care about the
  // challenge contents (it's bound to the credential, not the
  // assertion), but WebAuthn requires one.
  const challenge = crypto.getRandomValues(new Uint8Array(32))
  const userIdBytes = new TextEncoder().encode(userHandle)
  const cred = (await navigator.credentials.create({
    publicKey: {
      rp: { id: RP_ID, name: RP_NAME },
      user: {
        id: userIdBytes,
        name: userHandle,
        displayName: label,
      },
      challenge,
      pubKeyCredParams: [
        { type: 'public-key', alg: -7 }, // ES256
        { type: 'public-key', alg: -257 }, // RS256
      ],
      authenticatorSelection: {
        residentKey: 'preferred',
        userVerification: 'required',
      },
      timeout: 60_000,
      extensions: {
        prf: { eval: { first: salt } },
      } as AuthenticationExtensionsClientInputs,
    },
  })) as PublicKeyCredential | null
  if (!cred) throw new Error('WebAuthn returned no credential')
  const exts = cred.getClientExtensionResults() as AuthenticationExtensionsClientOutputs & {
    prf?: { enabled?: boolean; results?: { first?: ArrayBuffer } }
  }
  const prfFirst = exts.prf?.results?.first
  if (!prfFirst) {
    throw new Error(
      'Authenticator did not return a PRF output. Your hardware key or browser may not support WebAuthn PRF (hmac-secret).',
    )
  }
  return {
    credentialIdB64: bufToB64(cred.rawId),
    prfOutputB64: bufToB64(prfFirst),
    saltB64,
  }
}

/**
 * Run `credentials.get` against a previously-enrolled credential
 * and return its PRF output.  Used by the unlock flow (Phase 1B —
 * not wired yet at boot) and by the "Remove this key" confirm step
 * to prove the user can still authenticate before we drop a wrap.
 */
export async function evaluateFidoPrf(
  credentialIdB64: string,
  saltB64: string,
): Promise<string> {
  const credentialId = b64ToBuf(credentialIdB64)
  const salt = b64ToBuf(saltB64)
  const challenge = crypto.getRandomValues(new Uint8Array(32))
  const assertion = (await navigator.credentials.get({
    publicKey: {
      challenge,
      rpId: RP_ID,
      allowCredentials: [{ type: 'public-key', id: credentialId }],
      userVerification: 'required',
      timeout: 60_000,
      extensions: {
        prf: { eval: { first: salt } },
      } as AuthenticationExtensionsClientInputs,
    },
  })) as PublicKeyCredential | null
  if (!assertion) throw new Error('WebAuthn returned no assertion')
  const exts = assertion.getClientExtensionResults() as AuthenticationExtensionsClientOutputs & {
    prf?: { results?: { first?: ArrayBuffer } }
  }
  const prfFirst = exts.prf?.results?.first
  if (!prfFirst) {
    throw new Error('Authenticator did not return a PRF output')
  }
  return bufToB64(prfFirst)
}
