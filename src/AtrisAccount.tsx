import { FormEvent, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { CloudOff, LogIn, LogOut, ShieldCheck, UserRound, X } from "lucide-react";
import "./account.css";

interface AtrisUser {
  id: string;
  email: string;
  username: string;
  name?: string | null;
  role: string;
}

interface AtrisMembership {
  status: string;
  plan: string;
}

interface AuthSnapshot {
  state: "signed_out" | "signed_in" | "offline_cached";
  user?: AtrisUser | null;
  membership?: AtrisMembership | null;
  remembered: boolean;
  offline: boolean;
  message?: string | null;
}

const signedOut: AuthSnapshot = {
  state: "signed_out",
  user: null,
  membership: null,
  remembered: false,
  offline: false,
};

export default function AtrisAccount() {
  const [session, setSession] = useState<AuthSnapshot | null>(null);
  const [showLogin, setShowLogin] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [identity, setIdentity] = useState("");
  const [password, setPassword] = useState("");
  const [remember, setRemember] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void invoke<AuthSnapshot>("restore_atrishub_session")
      .then((value) => {
        if (cancelled) return;
        setSession(value);
        setShowLogin(value.state === "signed_out");
      })
      .catch((reason) => {
        if (cancelled) return;
        setSession(signedOut);
        setError(String(reason));
        setShowLogin(true);
      });
    return () => { cancelled = true; };
  }, []);

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!identity.trim() || !password || busy) return;
    setBusy(true);
    setError(null);
    try {
      const value = await invoke<AuthSnapshot>("login_atrishub", {
        email: identity.trim(),
        password,
        rememberDevice: remember,
      });
      setSession(value);
      setPassword("");
      setShowLogin(false);
    } catch (reason) {
      setPassword("");
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  async function logout() {
    setBusy(true);
    setError(null);
    try {
      setSession(await invoke<AuthSnapshot>("logout_atrishub"));
      setMenuOpen(false);
      setShowLogin(true);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  const user = session?.user;
  const label = user?.name?.trim() || user?.username || "Atris account";
  const initials = label
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((part) => part[0]?.toUpperCase())
    .join("") || "A";

  return (
    <>
      {session && session.state !== "signed_out" ? (
        <div className="account-anchor">
          <button className="account-chip" onClick={() => setMenuOpen((value) => !value)}>
            <span className="account-avatar">{initials}</span>
            <span className="account-chip-copy">
              <strong>{label}</strong>
              <small>{session.offline ? "Remembered · offline" : `AtrisHub · ${session.membership?.plan || "Member"}`}</small>
            </span>
            {session.offline ? <CloudOff size={14} /> : <ShieldCheck size={14} />}
          </button>
          {menuOpen && (
            <div className="account-menu">
              <div>
                <strong>{user?.email}</strong>
                <small>{session.remembered ? "This device is remembered" : "Session ends when AtrisBridge closes"}</small>
              </div>
              {session.message && <p>{session.message}</p>}
              <button onClick={() => void logout()} disabled={busy}><LogOut size={14} /> Sign out</button>
            </div>
          )}
        </div>
      ) : session ? (
        <button className="account-signin-pill" onClick={() => setShowLogin(true)}>
          <UserRound size={14} /> Sign in to AtrisHub
        </button>
      ) : null}

      {showLogin && (
        <div className="account-modal-backdrop" role="presentation">
          <section className="account-modal" role="dialog" aria-modal="true" aria-labelledby="atris-account-title">
            <button className="account-modal-close" onClick={() => setShowLogin(false)} aria-label="Continue offline">
              <X size={17} />
            </button>
            <div className="account-brand-mark">A</div>
            <div className="account-modal-heading">
              <span>ATRISHUB ACCOUNT</span>
              <h2 id="atris-account-title">Sign in to AtrisBridge</h2>
              <p>Use your atrishub.com account. Your password is sent directly to AtrisHub and is never stored on this computer.</p>
            </div>
            <form onSubmit={submit}>
              <label>
                <span>Email or username</span>
                <input
                  autoFocus
                  autoComplete="username"
                  value={identity}
                  onChange={(event) => setIdentity(event.target.value)}
                  placeholder="you@example.com"
                />
              </label>
              <label>
                <span>Password</span>
                <input
                  type="password"
                  autoComplete="current-password"
                  value={password}
                  onChange={(event) => setPassword(event.target.value)}
                  placeholder="••••••••"
                />
              </label>
              <label className="account-remember">
                <input type="checkbox" checked={remember} onChange={(event) => setRemember(event.target.checked)} />
                <span><strong>Remember this device</strong><small>Stores only a rotating refresh credential in your OS secure vault.</small></span>
              </label>
              {error && <div className="account-error">{error}</div>}
              <button className="account-login-button" type="submit" disabled={busy || !identity.trim() || !password}>
                <LogIn size={15} /> {busy ? "Signing in…" : "Sign in with AtrisHub"}
              </button>
            </form>
            <button className="account-offline-button" onClick={() => setShowLogin(false)} disabled={busy}>
              Continue with local AtrisBridge
            </button>
            <small className="account-local-note">AtrisBridge remains local-first. AtrisHub availability never blocks access to your local workspace data.</small>
          </section>
        </div>
      )}
    </>
  );
}
