package transport

import "testing"

func TestDesktopClientPolicyRequiresExactSamePackageAndDaemonPath(t *testing.T) {
	policy := desktopClientPolicy{
		executablePath: `C:\Program Files\WindowsApps\Grok\app\resources\bin\grok-daemon.exe`,
		packageFull:    "Grok_1.2.3.0_x64__publisher",
		packageFamily:  "Grok_publisher",
		packaged:       true,
	}
	if !policy.qualifies(
		`c:\program files\windowsapps\grok\app\resources\bin\GROK-DAEMON.EXE`,
		"grok_1.2.3.0_x64__PUBLISHER",
		"grok_PUBLISHER",
		true,
	) {
		t.Fatal("same packaged daemon was rejected")
	}
	tests := []struct {
		name      string
		path      string
		full      string
		family    string
		packaged  bool
		configure func(*desktopClientPolicy)
	}{
		{name: "unpackaged broker", path: policy.executablePath, full: policy.packageFull, family: policy.packageFamily, packaged: true,
			configure: func(candidate *desktopClientPolicy) { candidate.packaged = false }},
		{name: "unpackaged client", path: policy.executablePath, full: policy.packageFull, family: policy.packageFamily},
		{name: "wrong executable", path: `C:\Temp\grok-daemon.exe`, full: policy.packageFull, family: policy.packageFamily, packaged: true},
		{name: "wrong version", path: policy.executablePath, full: "Grok_1.2.2.0_x64__publisher", family: policy.packageFamily, packaged: true},
		{name: "wrong publisher family", path: policy.executablePath, full: policy.packageFull, family: "Grok_other", packaged: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			candidate := policy
			if test.configure != nil {
				test.configure(&candidate)
			}
			if candidate.qualifies(test.path, test.full, test.family, test.packaged) {
				t.Fatal("mismatched desktop process was qualified")
			}
		})
	}
}
