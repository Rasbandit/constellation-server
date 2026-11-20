import { useEffect, useMemo, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { api, liveRooms, HttpError } from '../api/client';
import type { ApWorld, CollectMode, ManifestWorld, PlayerFile, ReadinessCheck, ReleaseMode, RoomRules } from '../api/types';
import { Phases } from '../components/phases';
import { Card, Dropzone, Empty, Modal, useToast } from '../components/ui';
import { bytes } from '../lib/format';
import { gamesOf } from '../api/uploads';
import { jsonKind } from '../lib/jsonKind';
import { NAME_MAX, checkReadiness } from '../lib/readiness';

const defaultRules: RoomRules = { hintCostPct: 10, releaseMode: 'goal', collectMode: 'goal', password: '', spoilerLog: true };

const sampleYaml = (name: string, game: string) => `name: ${name}\ngame: ${game}\n${game}:\n  goal: default\n`;

export function CreateRoom() {
  const nav = useNavigate();
  const toast = useToast();
  const [name, setName] = useState('');
  const [players, setPlayers] = useState<PlayerFile[]>([]);
  const [worlds, setWorlds] = useState<ApWorld[]>([]);
  const [manifests, setManifests] = useState<ManifestWorld[]>([]);
  const [rules, setRules] = useState<RoomRules>(defaultRules);
  const [showRules, setShowRules] = useState(false);
  const [builtin, setBuiltin] = useState<string[]>([]);
  const [known, setKnown] = useState<string[]>([]);
  const [busy, setBusy] = useState(0);
  const [creating, setCreating] = useState(false);
  const [serverChecks, setServerChecks] = useState<ReadinessCheck[] | null>(null);
  const [adding, setAdding] = useState(false);

  useEffect(() => {
    api.games().then(
      (g) => {
        setBuiltin(g.builtin);
        setKnown(g.known);
      },
      () => undefined,
    );
  }, []);

  const readiness = useMemo(
    // A live room is named by its key; the name field is not shown then.
    () => checkReadiness({ roomName: liveRooms ? 'live' : name, apworlds: worlds, manifests, players, builtinGames: builtin }),
    [name, worlds, manifests, players, builtin],
  );
  const neededGames = useMemo(() => [...new Set(players.flatMap((p) => gamesOf(p.game)))].sort(), [players]);
  const gameChoices = useMemo(() => [...new Set([...builtin, ...known, ...worlds.map((w) => w.game), ...manifests.map((m) => m.game)])].sort(), [builtin, known, worlds, manifests]);

  async function track<T>(p: Promise<T>): Promise<T> {
    setBusy((b) => b + 1);
    try {
      return await p;
    } finally {
      setBusy((b) => b - 1);
    }
  }

  /** A `.json` file dropped in the wrong zone is refused here, before it is uploaded. */
  const refuseWrongZone = async (f: File, zone: 'manifest' | 'player') => {
    const kind = await jsonKind(f);
    if (kind === 'other' || kind === zone) return false;
    toast(`${f.name} looks like a ${kind === 'manifest' ? 'world manifest' : 'player file'}; drop it in the ${kind === 'manifest' ? 'manifest' : 'player'} area.`, 'error');
    return true;
  };

  const addPlayerFiles = (files: File[]) =>
    files.forEach(async (f) => {
      if (await refuseWrongZone(f, 'player')) return;
      track(api.uploadPlayerFile(f)).then(
        (added) => setPlayers((ps) => [...ps, ...added]),
        (e: Error) => toast(`${f.name}: ${e.message}`, 'error'),
      );
    });

  const addWorldFiles = (files: File[]) =>
    files.forEach((f) =>
      track(api.uploadApWorld(f)).then(
        (w) => setWorlds((ws) => [...ws, w]),
        (e: Error) => toast(`${f.name}: ${e.message}`, 'error'),
      ),
    );

  const addManifestFiles = (files: File[]) =>
    files.forEach(async (f) => {
      if (await refuseWrongZone(f, 'manifest')) return;
      track(api.uploadManifest(f)).then(
        (m) => setManifests((ms) => [...ms, m]),
        (e: Error) => toast(`${f.name}: ${e.message}`, 'error'),
      );
    });

  const updatePlayer = (id: string, patch: Partial<PlayerFile>) =>
    setPlayers((ps) => ps.map((p) => (p.id === id ? { ...p, ...patch, error: patch.game ? undefined : p.error } : p)));

  const loadSample = () => {
    addPlayerFiles([
      new File([sampleYaml('Kestrel', 'Starfall Keep')], 'kestrel.yaml'),
      new File([sampleYaml('Juniper', 'Tidewater Farm')], 'juniper.yaml'),
      new File([sampleYaml('Pathfinder', 'Archipela-Go')], 'pathfinder.yaml'),
    ]);
    addWorldFiles([new File(['stub'], 'starfall_keep-1.2.0.apworld')]);
    if (!name) setName('Sample room');
  };

  async function create() {
    setCreating(true);
    setServerChecks(null);
    try {
      const room = await api.createRoom({ name, apworlds: worlds, manifest: manifests[0] ?? null, players, rules });
      toast('Room created');
      nav(`/rooms/${room.id}/admin?key=${encodeURIComponent(room.adminKey ?? '')}&created=1`);
    } catch (e) {
      if (e instanceof HttpError && e.body.checks) setServerChecks(e.body.checks);
      else toast((e as Error).message, 'error');
    } finally {
      setCreating(false);
    }
  }

  return (
    <div className="create">
      <div className="page-head">
        <div>
          <h1>Create a room</h1>
          <p className="muted">Add files in any order. The checklist on the right shows what is left.</p>
        </div>
        {/* The sample files exist only in the stub; the server would refuse them. */}
        {!liveRooms && (
          <button type="button" className="btn btn-ghost" onClick={loadSample}>
            Try with sample files
          </button>
        )}
      </div>

      <Phases state="create" />

      <div className="create-grid">
        <div className="create-main">
          <Card title={<StepTitle n={1} text="Room" />}>
            {liveRooms ? (
              // The server cannot take a name, password or rules from this page yet, so none are asked for.
              <p className="muted">
                Not available yet: room name, password and rules. The room gets a key as its name; hint cost, release and collect
                come from the generated world, and a password from the server (CONSTELLATION_ROOM_PASSWORD).
              </p>
            ) : (
              <>
                <label className="field">
                  <span>Room name</span>
                  <input value={name} onChange={(e) => setName(e.target.value)} placeholder="Friday Night Multiworld" maxLength={60} />
                </label>
                <button type="button" className="disclosure" aria-expanded={showRules} onClick={() => setShowRules((s) => !s)}>
                  {showRules ? '▾' : '▸'} Rules: hint cost {rules.hintCostPct}%, release {rules.releaseMode}, collect {rules.collectMode}
                  {rules.password ? ', password set' : ''}
                </button>
                {showRules && <RulesForm rules={rules} onChange={setRules} />}
              </>
            )}
          </Card>

          <Card
            title={<StepTitle n={2} text="Players" />}
            actions={
              <button type="button" className="btn btn-primary btn-small" onClick={() => setAdding(true)}>
                Add game/player
              </button>
            }
          >
            <Dropzone
              accept=".yaml,.yml,.json"
              onFiles={addPlayerFiles}
              title="Drop player files"
              hint="YAML files, or JSON files with a name and a game for a manifest world. Several at once; a YAML file can hold several players. Or use Add game/player for one player with their files."
            />
            {players.length > 0 && (
              <div className="table-wrap">
                <table className="table players-table">
                  <thead>
                    <tr>
                      <th>Game</th>
                      <th>Player (connection name)</th>
                      <th>Options</th>
                      <th aria-label="Remove" />
                    </tr>
                  </thead>
                  <tbody>
                    {players.map((p) => {
                      const problem = readiness.nameProblems[p.id];
                      return (
                        <tr key={p.id}>
                          <td>
                            {p.defaults || p.error ? (
                              <select
                                value={p.game ?? ''}
                                aria-label="Game"
                                className={p.game ? '' : 'invalid'}
                                onChange={(e) => updatePlayer(p.id, { game: e.target.value || null })}
                              >
                                <option value="">Choose a game</option>
                                {gameChoices.map((g) => (
                                  <option key={g}>{g}</option>
                                ))}
                              </select>
                            ) : (
                              p.game
                            )}
                            {p.error && <div className="small error-text">{p.error}</div>}
                            {gamesOf(p.game).length > 1 && <div className="small muted">Weighted: the generator picks one</div>}
                            {gamesOf(p.game).map((g) => (
                              <WorldStatus key={g} game={g} label={gamesOf(p.game).length > 1 ? g : undefined} worlds={worlds} manifests={manifests} builtin={builtin} />
                            ))}
                          </td>
                          <td>
                            <input
                              className={problem ? 'invalid' : ''}
                              value={p.name}
                              aria-label="Connection name"
                              aria-invalid={!!problem}
                              onChange={(e) => updatePlayer(p.id, { name: e.target.value })}
                            />
                            <div className={`small ${problem ? 'error-text' : 'muted'}`}>
                              {problem ??
                                (p.yamlName && p.name !== p.yamlName
                                  ? `The YAML says "${p.yamlName}". Generation still uses the YAML's name.`
                                  : `${p.name.length}/${NAME_MAX}, what the player types to connect`)}
                            </div>
                          </td>
                          <td className="muted small">{p.fileName ?? 'Game defaults'}</td>
                          <td>
                            <button
                              type="button"
                              className="btn btn-ghost btn-small"
                              aria-label={`Remove ${p.name}`}
                              onClick={() => setPlayers((ps) => ps.filter((x) => x.id !== p.id))}
                            >
                              Remove
                            </button>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            )}
          </Card>

          <Card title={<StepTitle n={3} text="Game files (APWorlds)" />}>
            <div className="help">
              <p>
                An <strong>APWorld</strong> describes one game: its items, locations and rules. You need one per game, not per player;
                two players of the same game share it. Games built into the server need none.
              </p>
              <p>
                When you generate, the server combines every player's options with these files into one <strong>world file</strong>: a
                fixed record of where every item is. Every game in the room connects to it.
              </p>
            </div>
            {neededGames.length === 0 ? (
              <Empty>Add players first; this list shows which game files they need.</Empty>
            ) : (
              <ul className="needs">
                {neededGames.map((g) => {
                  const w = worlds.filter((x) => x.game === g);
                  const m = manifests.filter((x) => x.game === g);
                  const users = players.filter((p) => gamesOf(p.game).includes(g)).map((p) => p.name);
                  const state = w.length > 1 ? 'dup' : w.length === 1 ? 'ok' : m.length > 0 ? 'manifest' : builtin.includes(g) ? 'builtin' : 'missing';
                  return (
                    <li key={g} className={`need need-${state}`}>
                      <span className="need-icon" aria-hidden>
                        {state === 'ok' || state === 'builtin' || state === 'manifest' ? '✓' : '!'}
                      </span>
                      <div className="need-main">
                        <strong>{g}</strong>
                        <div className="muted small">Used by {users.join(', ')}</div>
                      </div>
                      <div className="need-state small">
                        {state === 'ok' && `${w[0].fileName} · v${w[0].version}`}
                        {state === 'manifest' && `${m[0].fileName} · v${m[0].version} (data-only)`}
                        {state === 'builtin' && 'Built into the server'}
                        {state === 'missing' && <span className="error-text">Missing</span>}
                        {state === 'dup' && <span className="error-text">{w.length} versions, keep one</span>}
                      </div>
                    </li>
                  );
                })}
              </ul>
            )}
            <Dropzone accept=".apworld" onFiles={addWorldFiles} title="Drop .apworld files" hint="The game and version are read from each file." />
            <Dropzone
              accept=".json"
              onFiles={addManifestFiles}
              title="Or drop a world manifest (.json)"
              hint="A data-only world: the server fills it itself and runs no world code. The game and version are read from the file."
            />
            {manifests.length > 0 && (
              <ul className="file-list">
                {manifests.map((m) => (
                  <li key={m.id}>
                    <div>
                      <strong>{m.fileName}</strong>
                      <span className="muted small">
                        {' '}
                        {m.game} · v{m.version} · {bytes(m.sizeBytes)}
                      </span>
                    </div>
                    <button type="button" className="btn btn-ghost btn-small" onClick={() => setManifests((ms) => ms.filter((x) => x.id !== m.id))}>
                      Remove
                    </button>
                  </li>
                ))}
              </ul>
            )}
            {worlds.length > 0 && (
              <ul className="file-list">
                {worlds.map((w) => (
                  <li key={w.id}>
                    <div>
                      <strong>{w.fileName}</strong>
                      <span className="muted small">
                        {' '}
                        {w.game} · v{w.version} · {bytes(w.sizeBytes)}
                      </span>
                      {!neededGames.includes(w.game) && <span className="tag-warn">not used</span>}
                    </div>
                    <button type="button" className="btn btn-ghost btn-small" onClick={() => setWorlds((ws) => ws.filter((x) => x.id !== w.id))}>
                      Remove
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </Card>
        </div>

        <aside className="create-side">
          <Card title="Checklist" className="sticky">
            <ul className="checklist">
              {readiness.checks.map((c) => (
                <li key={c.id} className={`check-${c.level}`}>
                  <span aria-hidden>{c.level === 'ok' ? '✓' : c.level === 'warning' ? '△' : '✕'}</span>
                  {c.message}
                </li>
              ))}
            </ul>
            {serverChecks && (
              <div className="callout error">
                The server refused:
                <ul>
                  {serverChecks.map((c) => (
                    <li key={c.id}>{c.message}</li>
                  ))}
                </ul>
              </div>
            )}
            <div className="summary small muted">
              {players.length} players · {neededGames.length} games · {worlds.length + manifests.length} files
              {busy > 0 && ` · uploading ${busy}...`}
            </div>
            <button
              type="button"
              className="btn btn-primary btn-block"
              disabled={!readiness.ready || busy > 0 || creating}
              onClick={create}
            >
              {creating ? 'Generating...' : 'Generate room'}
            </button>
            {!readiness.ready && <p className="small muted">Fix the items marked ✕ to continue.</p>}
          </Card>
        </aside>
      </div>

      <AddPlayerDialog
        open={adding}
        onClose={() => setAdding(false)}
        gameChoices={gameChoices}
        worlds={worlds}
        builtin={builtin}
        onAdd={(p, w) => {
          setPlayers((ps) => [...ps, ...p]);
          if (w) setWorlds((ws) => [...ws, w]);
        }}
      />
    </div>
  );
}

function WorldStatus({ game, label, worlds, manifests, builtin }: { game: string; label?: string; worlds: ApWorld[]; manifests: ManifestWorld[]; builtin: string[] }) {
  const n = worlds.filter((w) => w.game === game).length;
  const pre = label ? `${label}: ` : '';
  if (n === 1) return <div className="small muted">{pre}✓ APWorld uploaded</div>;
  if (n > 1) return <div className="small error-text">{pre}{n} APWorlds, keep one</div>;
  if (manifests.some((m) => m.game === game)) return <div className="small muted">{pre}✓ Manifest uploaded</div>;
  if (builtin.includes(game)) return <div className="small muted">{pre}✓ Built in</div>;
  return <div className="small error-text">{pre}APWorld missing</div>;
}

/** One player at a time: name, their YAML (or a game with defaults), and the game's APWorld if needed. */
function AddPlayerDialog({
  open,
  onClose,
  gameChoices,
  worlds,
  builtin,
  onAdd,
}: {
  open: boolean;
  onClose: () => void;
  gameChoices: string[];
  worlds: ApWorld[];
  builtin: string[];
  onAdd: (p: PlayerFile[], w: ApWorld | null) => void;
}) {
  const [name, setName] = useState('');
  const [yaml, setYaml] = useState<File | null>(null);
  const [game, setGame] = useState('');
  const [world, setWorld] = useState<File | null>(null);
  const [error, setError] = useState('');
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!open) {
      setName('');
      setYaml(null);
      setGame('');
      setWorld(null);
      setError('');
    }
  }, [open]);

  const covered = game !== '' && (builtin.includes(game) || worlds.some((w) => w.game === game));

  async function submit() {
    setError('');
    setSaving(true);
    try {
      const w = world ? await api.uploadApWorld(world) : null;
      let p: PlayerFile[];
      if (yaml) {
        p = await api.uploadPlayerFile(yaml);
        // A typed name renames a one-player file; a file with several players keeps theirs.
        if (name.trim() && p.length === 1) p = [{ ...p[0], name: name.trim() }];
      } else {
        const g = game || w?.game;
        if (!g) throw new Error('Choose a game, or add the player\'s YAML.');
        p = [{ id: `manual-${Date.now()}`, fileName: null, name: name.trim() || 'Player', game: g, defaults: true }];
      }
      onAdd(p, w);
      onClose();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setSaving(false);
    }
  }

  return (
    <Modal open={open} title="Add game/player" onClose={onClose}>
      <form
        className="form add-player"
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <label className="field">
          <span>Player name</span>
          <input value={name} onChange={(e) => setName(e.target.value)} maxLength={NAME_MAX} placeholder={yaml ? 'From the YAML unless you type one' : 'Kestrel'} autoFocus />
          <span className="small muted">What they type to connect. At most {NAME_MAX} characters.</span>
        </label>
        <label className="field">
          <span>Player YAML</span>
          <input type="file" accept=".yaml,.yml" onChange={(e) => setYaml(e.target.files?.[0] ?? null)} />
          <span className="small muted">Their options. Skip it to use the game's defaults.</span>
        </label>
        {!yaml && (
          <label className="field">
            <span>Game</span>
            <select value={game} onChange={(e) => setGame(e.target.value)}>
              <option value="">{world ? 'From the APWorld below' : 'Choose a game'}</option>
              {gameChoices.map((g) => (
                <option key={g}>{g}</option>
              ))}
            </select>
          </label>
        )}
        <label className="field">
          <span>APWorld</span>
          <input type="file" accept=".apworld" onChange={(e) => setWorld(e.target.files?.[0] ?? null)} />
          <span className="small muted">
            {covered ? `Not needed: ${game} is already covered.` : 'Needed once per game, unless the game is built in.'}
          </span>
        </label>
        {error && <p className="error-text small">{error}</p>}
        <div className="modal-actions">
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn btn-primary" disabled={saving || (!name.trim() && !yaml)}>
            {saving ? 'Adding...' : 'Add player'}
          </button>
        </div>
      </form>
    </Modal>
  );
}

function StepTitle({ n, text }: { n: number; text: string }) {
  return (
    <>
      <span className="step-no">{n}</span>
      {text}
    </>
  );
}

const releaseHelp: Record<ReleaseMode, string> = {
  disabled: 'Nobody can release.',
  enabled: 'Players can release any time.',
  auto: 'Released automatically on goal.',
  goal: 'Players can release after their goal.',
};

export function RulesForm({ rules, onChange }: { rules: RoomRules; onChange: (r: RoomRules) => void }) {
  const set = <K extends keyof RoomRules>(k: K, v: RoomRules[K]) => onChange({ ...rules, [k]: v });
  return (
    <div className="rules">
      <label className="field">
        <span>
          Hint cost: <strong>{rules.hintCostPct}%</strong> of a world's locations
        </span>
        <input type="range" min={0} max={100} step={5} value={rules.hintCostPct} onChange={(e) => set('hintCostPct', Number(e.target.value))} />
        <span className="small muted">{rules.hintCostPct === 0 ? 'Hints are free.' : 'Points come from checking locations.'}</span>
      </label>
      <div className="row">
        <label className="field">
          <span>Release</span>
          <select value={rules.releaseMode} onChange={(e) => set('releaseMode', e.target.value as ReleaseMode)}>
            {(['goal', 'enabled', 'auto', 'disabled'] as const).map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </select>
          <span className="small muted">{releaseHelp[rules.releaseMode]} Sends a player's remaining items to their owners.</span>
        </label>
        <label className="field">
          <span>Collect</span>
          <select value={rules.collectMode} onChange={(e) => set('collectMode', e.target.value as CollectMode)}>
            {(['goal', 'enabled', 'auto', 'disabled'] as const).map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </select>
          <span className="small muted">Gives a player their own items still in other worlds.</span>
        </label>
      </div>
      <div className="row">
        <label className="field">
          <span>Room password</span>
          <input type="text" value={rules.password} onChange={(e) => set('password', e.target.value)} placeholder="None" autoComplete="off" />
        </label>
        <label className="check field-check">
          <input type="checkbox" checked={rules.spoilerLog} onChange={(e) => set('spoilerLog', e.target.checked)} /> Keep a spoiler log
        </label>
      </div>
    </div>
  );
}
