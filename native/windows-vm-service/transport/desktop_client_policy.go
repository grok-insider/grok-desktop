package transport

import "strings"

type desktopClientPolicy struct {
	executablePath string
	packageFull    string
	packageFamily  string
	packaged       bool
}

func (policy desktopClientPolicy) qualifies(executablePath, packageFull, packageFamily string, packaged bool) bool {
	return policy.packaged && packaged &&
		strings.EqualFold(policy.executablePath, executablePath) &&
		strings.EqualFold(policy.packageFull, packageFull) &&
		strings.EqualFold(policy.packageFamily, packageFamily)
}
