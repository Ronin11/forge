// The mode registry is generated: go generate runs forge/internal/modes/gen,
// which lists the sibling mode packages of this directory and writes
// all/registry_gen.go (STYLE.md §1, §10). The generated list is a child
// package rather than this one because every mode package imports modes for
// the Mode interface, and a package cannot import its own importers. This
// file exists only to carry the directive; the interface lives in modes.go.

//go:generate go run forge/internal/modes/gen

package modes
