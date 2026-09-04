package flow

import (
	"fmt"
	"regexp"
	"strconv"
	"strings"
)

// Objective templates. A routine node's objective may reference the run and
// upstream outputs — {{run.objective}}, {{run.repositories}},
// {{steps.<node>.output.<dot.path>}}, {{steps.<node>.status}},
// {{steps.<node>.state}}, {{steps.<node>.summary}} — expanded by
// the engine just before the Work is created. Only `run.` and `steps.`
// references are the engine's: {{objective}} and {{repo}} pass through
// untouched for the ordinary prompt substitutions downstream. A missing path
// expands to "" and is reported, not failed — agents cope with a blank better
// than a run wedges on one.
var templateRef = regexp.MustCompile(`\{\{\s*((?:run|steps)\.[A-Za-z0-9_.\-]+)\s*\}\}`)

// ExpandTemplate resolves every run/steps reference in s against the input,
// returning the expansion and the references that resolved to nothing.
func ExpandTemplate(s string, input ScriptInput) (string, []string) {
	var missing []string
	out := templateRef.ReplaceAllStringFunc(s, func(m string) string {
		ref := templateRef.FindStringSubmatch(m)[1]
		val, ok := resolveRef(ref, input)
		if !ok {
			missing = append(missing, ref)
			return ""
		}
		return val
	})
	return out, missing
}

func resolveRef(ref string, input ScriptInput) (string, bool) {
	parts := strings.Split(ref, ".")
	switch parts[0] {
	case "run":
		if len(parts) != 2 {
			return "", false
		}
		switch parts[1] {
		case "objective":
			return input.Run.Objective, input.Run.Objective != ""
		case "repositories":
			return strings.Join(input.Run.Repositories, ", "), len(input.Run.Repositories) > 0
		case "workflow":
			return input.Run.Workflow, true
		case "id":
			return input.Run.ID, true
		}
		return "", false
	case "steps":
		if len(parts) < 3 {
			return "", false
		}
		step, ok := input.Steps[parts[1]]
		if !ok {
			return "", false
		}
		switch parts[2] {
		case "status":
			return step.Status, true
		case "state":
			return step.State, step.State != ""
		case "summary":
			return step.Summary, step.Summary != ""
		case "output":
			return resolvePath(step.Output, parts[3:])
		}
		return "", false
	}
	return "", false
}

// resolvePath walks decoded JSON by keys; scalars render plainly, anything
// structured renders as its JSON-ish Go form.
func resolvePath(v any, path []string) (string, bool) {
	for _, key := range path {
		m, ok := v.(map[string]any)
		if !ok {
			return "", false
		}
		if v, ok = m[key]; !ok {
			return "", false
		}
	}
	switch t := v.(type) {
	case nil:
		return "", false
	case string:
		return t, true
	case float64:
		return strconv.FormatFloat(t, 'g', -1, 64), true
	case bool:
		return fmt.Sprintf("%t", t), true
	default:
		return fmt.Sprintf("%v", t), true
	}
}
