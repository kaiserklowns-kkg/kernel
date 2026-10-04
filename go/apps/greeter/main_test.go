package main

import "testing"

func TestDescribeArgs(t *testing.T) {
	for _, c := range []struct {
		args []string
		want string
	}{
		{nil, "no arguments"},
		{[]string{"one"}, `1 argument: "one"`},
		{[]string{"alpha", "beta"}, `2 arguments: "alpha" "beta"`},
	} {
		if got := describeArgs(c.args); got != c.want {
			t.Errorf("%q: got %q, want %q", c.args, got, c.want)
		}
	}
}
