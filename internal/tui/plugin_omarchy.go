package tui

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// The Omarchy side of the omarchy-indicator plugin (DESIGN.md §17): the QML
// under plugins/omarchy-indicator/ is copied into the Omarchy plugin
// directory and the widget is placed on the bar's right section, immediately
// before omarchy.agents. cmd_plugin.go calls the two exported-to-package
// functions; everything else here is the file copy and the shell.json edit,
// factored so tests never touch the real home.

// omarchyPluginID is the widget id in manifest.json and shell.json.
const omarchyPluginID = "ronin.forge"

// omarchyAgentsID is the stock widget the indicator is placed before.
const omarchyAgentsID = "omarchy.agents"

// installOmarchyIndicator copies the plugin's QML into
// ~/.config/omarchy/plugins/ronin.forge/ and adds the widget to the bar.
func installOmarchyIndicator(ctx context.Context, c *Context) error {
	src, err := omarchyIndicatorSource()
	if err != nil {
		return fmt.Errorf("locate omarchy-indicator source: %w", err)
	}
	dst := filepath.Join(omarchyConfigDir(c), "plugins", omarchyPluginID)
	copied, err := copyOmarchyPluginFiles(src, dst)
	if err != nil {
		return fmt.Errorf("install plugin files: %w", err)
	}
	fmt.Fprintf(c.Stdout, "installed %s into %s\n", strings.Join(copied, ", "), dst)

	shellJSON := filepath.Join(omarchyConfigDir(c), "shell.json")
	present, err := shellJSONHasEntry(shellJSON, omarchyPluginID)
	if err != nil {
		return fmt.Errorf("read %s: %w", shellJSON, err)
	}
	if present {
		fmt.Fprintf(c.Stdout, "%s is already on the bar\n", omarchyPluginID)
		return nil
	}

	// Prefer asking the shell: `omarchy bar put` places the widget through
	// the running shell's own config handling and hot-reloads it.
	if omarchyBarPutAvailable(ctx) {
		out, err := exec.CommandContext(ctx, "omarchy", "bar", "put", omarchyPluginID,
			"--section", "right", "--before", omarchyAgentsID).CombinedOutput()
		if err == nil {
			fmt.Fprintf(c.Stdout, "omarchy bar put: %s\n", strings.TrimSpace(string(out)))
			return nil
		}
		fmt.Fprintf(c.Stderr, "omarchy bar put failed (%v): %s — editing shell.json directly\n",
			err, strings.TrimSpace(string(out)))
	}

	// Fall back to editing shell.json. A missing file means the shell still
	// runs on its built-in defaults; writing one with only our entry would
	// replace the whole default bar, so leave it alone and say so.
	if _, err := os.Stat(shellJSON); os.IsNotExist(err) {
		fmt.Fprintf(c.Stdout, "%s does not exist; not creating one — run: omarchy bar put %s --section right --before %s\n",
			shellJSON, omarchyPluginID, omarchyAgentsID)
		return nil
	}
	backup := fmt.Sprintf("%s.forge-backup-%d", shellJSON, c.Now().Unix())
	changed, err := addOmarchyBarEntry(shellJSON, backup)
	if err != nil {
		return fmt.Errorf("add %s to %s: %w", omarchyPluginID, shellJSON, err)
	}
	if changed {
		fmt.Fprintf(c.Stdout, "backed up %s to %s\n", shellJSON, backup)
		fmt.Fprintf(c.Stdout, "added %s to the bar's right section\n", omarchyPluginID)
	} else {
		fmt.Fprintf(c.Stdout, "%s is already on the bar\n", omarchyPluginID)
	}
	return nil
}

// uninstallOmarchyIndicator removes the widget from shell.json and deletes
// the installed plugin directory. Safe when either is already gone.
func uninstallOmarchyIndicator(ctx context.Context, c *Context) error {
	shellJSON := filepath.Join(omarchyConfigDir(c), "shell.json")
	if _, err := os.Stat(shellJSON); err == nil {
		backup := fmt.Sprintf("%s.forge-backup-%d", shellJSON, c.Now().Unix())
		changed, err := removeOmarchyBarEntry(shellJSON, backup)
		if err != nil {
			return fmt.Errorf("remove %s from %s: %w", omarchyPluginID, shellJSON, err)
		}
		if changed {
			fmt.Fprintf(c.Stdout, "backed up %s to %s\n", shellJSON, backup)
			fmt.Fprintf(c.Stdout, "removed %s from the bar\n", omarchyPluginID)
		} else {
			fmt.Fprintf(c.Stdout, "%s was not on the bar\n", omarchyPluginID)
		}
	}

	dst := filepath.Join(omarchyConfigDir(c), "plugins", omarchyPluginID)
	if _, err := os.Stat(dst); os.IsNotExist(err) {
		fmt.Fprintf(c.Stdout, "%s is not installed\n", dst)
		return nil
	}
	if err := os.RemoveAll(dst); err != nil {
		return fmt.Errorf("remove %s: %w", dst, err)
	}
	fmt.Fprintf(c.Stdout, "removed %s\n", dst)
	return nil
}

// omarchyConfigDir is ~/.config/omarchy, honoring XDG_CONFIG_HOME the way
// serviceUnitDir does.
func omarchyConfigDir(c *Context) string {
	if x := c.Getenv("XDG_CONFIG_HOME"); x != "" {
		return filepath.Join(x, "omarchy")
	}
	return filepath.Join(c.UserHome, ".config", "omarchy")
}

// omarchyIndicatorSource walks up from the forge binary (and, failing that,
// the working directory) to the repository checkout that carries
// plugins/omarchy-indicator/plugin.toml — the same way cmd_plugin.go locates
// first-party plugins.
func omarchyIndicatorSource() (string, error) {
	var starts []string
	if exe, err := os.Executable(); err == nil {
		if resolved, err := filepath.EvalSymlinks(exe); err == nil {
			exe = resolved
		}
		starts = append(starts, filepath.Dir(exe))
	}
	if wd, err := os.Getwd(); err == nil {
		starts = append(starts, wd)
	}
	for _, start := range starts {
		for dir := start; ; {
			candidate := filepath.Join(dir, "plugins", "omarchy-indicator")
			if _, err := os.Stat(filepath.Join(candidate, "plugin.toml")); err == nil {
				return candidate, nil
			}
			parent := filepath.Dir(dir)
			if parent == dir {
				break
			}
			dir = parent
		}
	}
	return "", fmt.Errorf("plugins/omarchy-indicator/plugin.toml not found above %s", strings.Join(starts, " or "))
}

// copyOmarchyPluginFiles copies the Omarchy-side files (manifest.json, every
// *.qml, README.md — not plugin.toml, which belongs to Forge) from src into
// dst, overwriting freely so a reinstall updates in place. Returns the names
// it copied, sorted by directory order.
func copyOmarchyPluginFiles(src, dst string) ([]string, error) {
	entries, err := os.ReadDir(src)
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", src, err)
	}
	if err := os.MkdirAll(dst, 0o755); err != nil {
		return nil, fmt.Errorf("create %s: %w", dst, err)
	}
	var copied []string
	for _, e := range entries {
		if e.IsDir() {
			continue
		}
		name := e.Name()
		if name != "manifest.json" && name != "README.md" && !strings.HasSuffix(name, ".qml") {
			continue
		}
		data, err := os.ReadFile(filepath.Join(src, name))
		if err != nil {
			return nil, fmt.Errorf("read %s: %w", filepath.Join(src, name), err)
		}
		if err := os.WriteFile(filepath.Join(dst, name), data, 0o644); err != nil {
			return nil, fmt.Errorf("write %s: %w", filepath.Join(dst, name), err)
		}
		copied = append(copied, name)
	}
	if len(copied) == 0 {
		return nil, fmt.Errorf("no plugin files found in %s", src)
	}
	return copied, nil
}

// omarchyBarPutAvailable reports whether `omarchy bar put` exists: the
// omarchy binary is on PATH and its bar help lists the put subcommand.
func omarchyBarPutAvailable(ctx context.Context) bool {
	if _, err := exec.LookPath("omarchy"); err != nil {
		return false
	}
	out, err := exec.CommandContext(ctx, "omarchy", "bar", "--help").CombinedOutput()
	return err == nil && strings.Contains(string(out), "put <id>")
}

// shellJSONHasEntry reports whether any bar layout section carries the id.
// A missing file simply has no entry.
func shellJSONHasEntry(path, id string) (bool, error) {
	raw, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	var doc map[string]any
	if err := json.Unmarshal(raw, &doc); err != nil {
		return false, fmt.Errorf("parse %s: %w", path, err)
	}
	layout := shellBarLayout(doc, false)
	for _, section := range []string{"left", "center", "right"} {
		for _, e := range shellLayoutSection(layout, section) {
			if shellEntryID(e) == id {
				return true, nil
			}
		}
	}
	return false, nil
}

// addOmarchyBarEntry inserts {"id": "ronin.forge"} into bar.layout.right,
// immediately before omarchy.agents (appending when it is absent). No-op
// when the id is already anywhere on the bar. Backs the file up to
// backupPath before the first change and writes atomically.
func addOmarchyBarEntry(path, backupPath string) (bool, error) {
	return editShellJSON(path, backupPath, func(doc map[string]any) bool {
		layout := shellBarLayout(doc, true)
		for _, section := range []string{"left", "center", "right"} {
			for _, e := range shellLayoutSection(layout, section) {
				if shellEntryID(e) == omarchyPluginID {
					return false
				}
			}
		}
		right := shellLayoutSection(layout, "right")
		entry := map[string]any{"id": omarchyPluginID}
		at := -1
		for i, e := range right {
			if shellEntryID(e) == omarchyAgentsID {
				at = i
				break
			}
		}
		if at < 0 {
			right = append(right, entry)
		} else {
			right = append(right[:at], append([]any{entry}, right[at:]...)...)
		}
		layout["right"] = right
		return true
	})
}

// removeOmarchyBarEntry removes every ronin.forge entry from the bar layout.
// Backs the file up to backupPath before the first change and writes
// atomically; a layout without the entry is left untouched.
func removeOmarchyBarEntry(path, backupPath string) (bool, error) {
	return editShellJSON(path, backupPath, func(doc map[string]any) bool {
		layout := shellBarLayout(doc, false)
		if layout == nil {
			return false
		}
		changed := false
		for _, section := range []string{"left", "center", "right"} {
			entries := shellLayoutSection(layout, section)
			if entries == nil {
				continue
			}
			kept := entries[:0:0]
			for _, e := range entries {
				if shellEntryID(e) == omarchyPluginID {
					changed = true
					continue
				}
				kept = append(kept, e)
			}
			if len(kept) != len(entries) {
				layout[section] = kept
			}
		}
		return changed
	})
}

// editShellJSON reads path, applies edit, and — only when edit reports a
// change — copies the original bytes to backupPath and rewrites the file
// atomically (tmp + rename, two-space indentation like the shell's own).
func editShellJSON(path, backupPath string, edit func(doc map[string]any) bool) (bool, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return false, err
	}
	var doc map[string]any
	if err := json.Unmarshal(raw, &doc); err != nil {
		return false, fmt.Errorf("parse %s: %w", path, err)
	}
	if !edit(doc) {
		return false, nil
	}
	if err := os.WriteFile(backupPath, raw, 0o644); err != nil {
		return false, fmt.Errorf("back up to %s: %w", backupPath, err)
	}
	out, err := json.MarshalIndent(doc, "", "  ")
	if err != nil {
		return false, fmt.Errorf("encode %s: %w", path, err)
	}
	out = append(out, '\n')
	tmp, err := os.CreateTemp(filepath.Dir(path), ".shell.json.forge-*")
	if err != nil {
		return false, fmt.Errorf("create temp file: %w", err)
	}
	tmpName := tmp.Name()
	discard := func(err error) []error {
		// Best-effort cleanup of the temp file on a failed write: surfaced by
		// joining, never silently blanked (STYLE: errcheck -blank).
		var errs []error
		if err != nil {
			errs = append(errs, err)
		}
		return errs
	}
	if _, werr := tmp.Write(out); werr != nil {
		errs := append([]error{fmt.Errorf("write %s: %w", tmpName, werr)}, discard(tmp.Close())...)
		errs = append(errs, discard(os.Remove(tmpName))...)
		return false, errors.Join(errs...)
	}
	if cerr := tmp.Chmod(0o644); cerr != nil {
		errs := append([]error{fmt.Errorf("chmod %s: %w", tmpName, cerr)}, discard(tmp.Close())...)
		errs = append(errs, discard(os.Remove(tmpName))...)
		return false, errors.Join(errs...)
	}
	if cerr := tmp.Close(); cerr != nil {
		return false, errors.Join(fmt.Errorf("close %s: %w", tmpName, cerr), errors.Join(discard(os.Remove(tmpName))...))
	}
	if rerr := os.Rename(tmpName, path); rerr != nil {
		return false, errors.Join(fmt.Errorf("rename %s to %s: %w", tmpName, path, rerr), errors.Join(discard(os.Remove(tmpName))...))
	}
	return true, nil
}

// shellBarLayout returns bar.layout, creating the path when create is set.
func shellBarLayout(doc map[string]any, create bool) map[string]any {
	bar, ok := doc["bar"].(map[string]any)
	if !ok {
		if !create {
			return nil
		}
		bar = map[string]any{}
		doc["bar"] = bar
	}
	layout, ok := bar["layout"].(map[string]any)
	if !ok {
		if !create {
			return nil
		}
		layout = map[string]any{}
		bar["layout"] = layout
	}
	return layout
}

// shellLayoutSection returns one section's entries; nil when absent.
func shellLayoutSection(layout map[string]any, name string) []any {
	if layout == nil {
		return nil
	}
	entries, ok := layout[name].([]any)
	if !ok {
		return nil
	}
	return entries
}

// shellEntryID is the widget id of a layout entry. The shell accepts both
// {"id": "x"} objects and bare "x" strings (see Util.normalizeLayoutEntry).
func shellEntryID(entry any) string {
	switch v := entry.(type) {
	case string:
		return v
	case map[string]any:
		id, ok := v["id"].(string)
		if !ok {
			return ""
		}
		return id
	}
	return ""
}
