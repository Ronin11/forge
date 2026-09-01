package worker

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"

	"forge/internal/core/model"
	"forge/internal/core/protocol"
)

// The virtual greenfield repository (MODES.md §greenfield): advertised when
// [greenfield] projects_root is configured, never a checkout. The reserved
// name cannot be configured as a real repository.
const (
	greenfieldRepoName       = "greenfield"
	greenfieldOriginIdentity = "greenfield:local"
)

// Artifact caps: what one attempt may hand back from its artifacts directory.
const (
	maxArtifactFiles = 50
	maxArtifactBytes = 50 << 20
)

// isGreenfield reports whether this attempt runs in the virtual repository.
func (a *attempt) isGreenfield() bool {
	return a.repo != nil && a.repo.OriginIdentity == greenfieldOriginIdentity
}

// prepareGreenfield replaces steps 2–4 for the virtual repository: fetch and
// resolve_base are skipped (their spans record it), and worktree_add is a
// fresh `git init` plus an empty initial commit under
// <data_dir>/greenfield/<attempt-id> — the directory is the product.
func (a *attempt) prepareGreenfield(ctx context.Context) error {
	c := a.claim
	for _, name := range []string{"fetch", "resolve_base"} {
		span := a.emitter.StartSpan(name, name, "", nil)
		span.End(nil, map[string]any{"skipped": true})
	}
	a.heartbeat(ctx, protocol.HeartbeatRequest{State: model.Preparing, Phase: "resolve_base"})
	dir := filepath.Join(a.r.cfg.DataDir, "greenfield", c.AttemptID)
	span := a.emitter.StartSpan("worktree_add", "worktree_add", "", nil)
	err := a.initGreenfield(ctx, dir)
	span.End(err, map[string]any{"path": dir, "branch": "main"})
	if err != nil {
		return fmt.Errorf("greenfield init for %s: %w", c.AttemptID, err)
	}
	return nil
}

// initGreenfield writes the intent manifest (kind greenfield) before the
// mutation, then creates the project directory as its own repository. The
// author is forge-greenfield@localhost so commits work on any machine.
func (a *attempt) initGreenfield(ctx context.Context, dir string) error {
	c := a.claim
	if _, err := os.Lstat(dir); err == nil {
		return errors.New("path already exists")
	} else if !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("inspect path: %w", err)
	}
	m := &Manifest{
		AttemptID: c.AttemptID, TargetID: c.TargetID, WorkID: c.WorkID, RoutineName: c.RoutineName,
		RepositoryName: greenfieldRepoName, RepositoryPath: a.repo.Path, OriginIdentity: greenfieldOriginIdentity,
		BaseBranch: "main", WorktreePath: dir, Branch: "main", Kind: manifestKindGreenfield,
		Lifecycle: ManifestPreparing,
	}
	if err := a.r.manifests.Write(m); err != nil {
		return err
	}
	a.manifest = m
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return fmt.Errorf("create project dir: %w", err)
	}
	g := a.r.git
	ident := []string{"-c", "user.name=forge-greenfield", "-c", "user.email=forge-greenfield@localhost"}
	if _, err := g.Run(ctx, dir, "init", "-q", "-b", "main"); err != nil {
		return err
	}
	if _, err := g.Run(ctx, dir, append(ident, "commit", "-q", "--allow-empty", "-m", "forge: greenfield init")...); err != nil {
		return err
	}
	out, err := g.Run(ctx, dir, "rev-parse", "--verify", "HEAD^{commit}")
	if err != nil {
		return err
	}
	commit := strings.TrimSpace(out)
	if !isFullCommit(commit) {
		return fmt.Errorf("init commit %q is not a full commit ID", commit)
	}
	m.BaseCommit = commit
	return a.r.manifests.Write(m)
}

// greenfieldMove renames a succeeded greenfield project to
// <projects_root>/<slug(project_name)>. A bad slug or an existing destination
// refuses the move and retains the project in place; either way the manifest
// stays the one record of where the directory lives.
func (a *attempt) greenfieldMove(result json.RawMessage) {
	m := a.manifest
	root := a.r.cfg.Greenfield.ProjectsRoot
	if m == nil || m.Kind != manifestKindGreenfield || root == "" || len(result) == 0 {
		return
	}
	var res struct {
		ProjectPath string `json:"project_path"`
		ProjectName string `json:"project_name"`
	}
	if err := json.Unmarshal(result, &res); err != nil || res.ProjectName == "" {
		return
	}
	refuse := func(why string) {
		a.emitter.Lifecycle("greenfield move refused; project retained in place", map[string]any{"project_name": res.ProjectName, "path": m.WorktreePath, "error": why})
	}
	slug := slugify(res.ProjectName)
	if err := model.ValidateName(slug); err != nil {
		refuse(err.Error())
		return
	}
	dest := filepath.Join(root, slug)
	if _, err := os.Lstat(dest); err == nil {
		refuse("destination " + dest + " already exists")
		return
	} else if !errors.Is(err, os.ErrNotExist) {
		refuse("inspect destination: " + err.Error())
		return
	}
	if err := os.MkdirAll(root, 0o700); err != nil {
		refuse("create projects root: " + err.Error())
		return
	}
	if err := os.Rename(m.WorktreePath, dest); err != nil {
		refuse(err.Error())
		return
	}
	// Flatten if the agent built in a subdirectory named after the project: when
	// dest/<slug>/ is the only non-hidden directory, move its contents up.
	nested := filepath.Join(dest, slug)
	if stat, err := os.Stat(nested); err == nil && stat.IsDir() {
		entries, err := os.ReadDir(dest)
		if err == nil {
			nonHiddenDirs := 0
			for _, e := range entries {
				if e.IsDir() && !strings.HasPrefix(e.Name(), ".") {
					nonHiddenDirs++
				}
			}
			if nonHiddenDirs == 1 {
				nestedEntries, err := os.ReadDir(nested)
				if err == nil {
					allMoved := true
					for _, ne := range nestedEntries {
						src := filepath.Join(nested, ne.Name())
						dst := filepath.Join(dest, ne.Name())
						if err := os.Rename(src, dst); err != nil {
							allMoved = false
							break
						}
					}
					if allMoved {
						if err := os.Remove(nested); err != nil {
							// The flatten already succeeded; an unremovable
							// shell dir (stray dotfiles) is only worth a note.
							a.emitter.Lifecycle("greenfield flatten left the empty shell directory", map[string]any{"path": nested, "error": err.Error()})
						}
					}
				}
			}
		}
	}
	m.WorktreePath = dest
	a.writeManifest(a.ctx, m)
	a.emitter.Lifecycle("greenfield project moved", map[string]any{"moved_to": dest})
}

// slugify reduces a project name to the safe name alphabet; the result must
// still pass model.ValidateName or the move is refused.
func slugify(name string) string {
	var b []rune
	for _, r := range strings.ToLower(strings.TrimSpace(name)) {
		switch {
		case r >= 'a' && r <= 'z', r >= '0' && r <= '9':
			b = append(b, r)
		default:
			if len(b) > 0 && b[len(b)-1] != '-' {
				b = append(b, '-')
			}
		}
	}
	for len(b) > 0 && b[len(b)-1] == '-' {
		b = b[:len(b)-1]
	}
	return string(b)
}

// artifactsDir is what FORGE_ARTIFACTS points at; created lazily so every
// attempt has somewhere to write screenshots and logs (VERIFICATION.md L2).
func (a *attempt) artifactsDir() string {
	dir := filepath.Join(a.r.cfg.DataDir, "artifacts", a.claim.AttemptID)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		a.log.WarnContext(a.ctx, "create artifacts dir", "error", err)
	}
	return dir
}

// collectArtifacts scans the artifacts directory after the agent exits: each
// regular file becomes one upload (kind by extension), capped at
// maxArtifactFiles / maxArtifactBytes; files beyond the cap are skipped with a
// lifecycle event, never silently.
func (a *attempt) collectArtifacts() []protocol.ArtifactUpload {
	dir := filepath.Join(a.r.cfg.DataDir, "artifacts", a.claim.AttemptID)
	entries, err := os.ReadDir(dir)
	if err != nil || len(entries) == 0 {
		return nil
	}
	var out []protocol.ArtifactUpload
	var total int64
	skipped := 0
	for _, e := range entries {
		if !e.Type().IsRegular() {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		if len(out) >= maxArtifactFiles || total+info.Size() > maxArtifactBytes {
			skipped++
			continue
		}
		p := filepath.Join(dir, e.Name())
		sum, err := sha256File(p)
		if err != nil {
			a.log.WarnContext(a.ctx, "hash artifact", "path", p, "error", err)
			continue
		}
		kind := "file"
		if strings.EqualFold(filepath.Ext(e.Name()), ".png") {
			kind = "screenshot"
		}
		out = append(out, protocol.ArtifactUpload{Kind: kind, Path: p, Bytes: info.Size(), SHA256: sum})
		total += info.Size()
	}
	if skipped > 0 {
		a.emitter.Lifecycle("artifacts beyond cap skipped", map[string]any{"skipped": skipped, "kept": len(out), "cap_files": maxArtifactFiles, "cap_bytes": maxArtifactBytes})
	}
	return out
}

func sha256File(path string) (sum string, err error) {
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer func() {
		if cerr := f.Close(); cerr != nil && err == nil {
			err = cerr
		}
	}()
	h := sha256.New()
	if _, err := io.Copy(h, f); err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}
