export const windowsGoArchitectures = Object.freeze({ x64: "amd64", arm64: "arm64" });

export function createWindowsGoBuildEnvironment(environment, architecture) {
  if (!Object.hasOwn(windowsGoArchitectures, architecture)) {
    throw new Error("Windows Go build architecture is unsupported");
  }
  const result = {
    GOOS: "windows",
    GOARCH: windowsGoArchitectures[architecture],
    CGO_ENABLED: "0",
    GOAUTH: "off",
    GOENV: "off",
    GOFLAGS: "",
    GONOPROXY: "",
    GONOSUMDB: "",
    GOPRIVATE: "",
    GOPROXY: "https://proxy.golang.org",
    GOSUMDB: "sum.golang.org",
    GOTOOLCHAIN: "local",
    GOWORK: "off",
  };
  for (const name of [
    "GOCACHE", "GOMODCACHE", "GOPATH", "HOME", "LOCALAPPDATA", "SystemRoot",
    "TEMP", "TMP", "USERPROFILE", "WINDIR",
  ]) {
    if (typeof environment[name] === "string" && environment[name].length > 0 &&
        !environment[name].includes("\0")) {
      result[name] = environment[name];
    }
  }
  return result;
}
