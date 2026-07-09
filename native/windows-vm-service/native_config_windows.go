//go:build windows

package vmservice

func validateNativeConfig(config normalizedConfig) error {
	for _, entry := range []struct{ name, root string }{
		{"ImageRoot", config.imageRoot},
		{"WorkspaceRoot", config.workspaceRoot},
	} {
		name, root := entry.name, entry.root
		volume := root
		if len(volume) > 2 {
			volume = volume[:2]
		}
		if len(volume) != 2 || volume[1] != ':' || !((volume[0] >= 'A' && volume[0] <= 'Z') || (volume[0] >= 'a' && volume[0] <= 'z')) {
			return serviceError(CodeInvalidArgument, "%s must use a local drive path", name)
		}
	}
	return nil
}
