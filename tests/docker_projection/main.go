// Exercise the exact inspect template with Docker's missingkey=error behavior.
package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"text/template"
)

type request struct {
	Template string            `json:"template"`
	Fixtures []json.RawMessage `json:"fixtures"`
}
type result struct {
	Output string `json:"output"`
	Error  string `json:"error"`
}

func main() {
	var req request
	if err := json.NewDecoder(os.Stdin).Decode(&req); err != nil {
		panic(err)
	}
	t, err := template.New("inspect").Option("missingkey=error").Funcs(template.FuncMap{
		"json": func(v any) string {
			b, err := json.Marshal(v)
			if err != nil {
				panic(err)
			}
			return string(b)
		},
	}).Parse(req.Template)
	if err != nil {
		panic(err)
	}
	out := make([]result, 0, len(req.Fixtures))
	for _, fixture := range req.Fixtures {
		var data map[string]any
		if err := json.Unmarshal(fixture, &data); err != nil {
			panic(err)
		}
		var buf bytes.Buffer
		err := t.Execute(&buf, data)
		row := result{Output: buf.String()}
		if err != nil {
			row.Error = err.Error()
		}
		out = append(out, row)
	}
	if err := json.NewEncoder(os.Stdout).Encode(out); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
