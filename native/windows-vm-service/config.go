package vmservice

import "path/filepath"

type Config struct {
	CurrentUserSID        string
	ImageRoot             string
	WorkspaceRoot         string
	AllowedSocketPurposes []SocketPurpose
	GuestControlMaxBytes  int
	GuestImagePolicy      *GuestImagePolicy
}

type normalizedConfig struct {
	currentUserSID        string
	imageRoot             string
	workspaceRoot         string
	stateRoot             string
	installedRoot         string
	vmRoot                string
	allowedSocketPurposes map[SocketPurpose]struct{}
	guestControlMaxBytes  int
	guestImagePolicy      *GuestImagePolicy
}

func normalizeConfig(config Config) (normalizedConfig, error) {
	if !sidPattern.MatchString(config.CurrentUserSID) {
		return normalizedConfig{}, serviceError(CodeInvalidArgument, "CurrentUserSID is not a valid Windows SID")
	}
	imageRoot, err := normalizeRoot("ImageRoot", config.ImageRoot)
	if err != nil {
		return normalizedConfig{}, err
	}
	workspaceRoot, err := normalizeRoot("WorkspaceRoot", config.WorkspaceRoot)
	if err != nil {
		return normalizedConfig{}, err
	}
	if rootsOverlap(imageRoot, workspaceRoot) {
		return normalizedConfig{}, serviceError(CodeInvalidArgument, "ImageRoot and WorkspaceRoot must be disjoint")
	}

	purposes := config.AllowedSocketPurposes
	if len(purposes) == 0 {
		purposes = []SocketPurpose{SocketPurposeControl, SocketPurposeComputerUseV1}
	}
	allowed := make(map[SocketPurpose]struct{}, len(purposes))
	for _, purpose := range purposes {
		if err := validateSocketPurpose(purpose); err != nil {
			return normalizedConfig{}, err
		}
		allowed[purpose] = struct{}{}
	}
	guestControlMaxBytes := config.GuestControlMaxBytes
	if guestControlMaxBytes == 0 {
		guestControlMaxBytes = DefaultGuestControlMaxBytes
	}
	if guestControlMaxBytes < 4096 || guestControlMaxBytes > DefaultGuestControlMaxBytes {
		return normalizedConfig{}, serviceError(
			CodeInvalidArgument,
			"GuestControlMaxBytes must be between 4096 and %d",
			DefaultGuestControlMaxBytes,
		)
	}

	return normalizedConfig{
		currentUserSID:        config.CurrentUserSID,
		imageRoot:             imageRoot,
		workspaceRoot:         workspaceRoot,
		stateRoot:             filepath.Join(imageRoot, ".vm-service"),
		installedRoot:         filepath.Join(imageRoot, ".vm-service", "installed"),
		vmRoot:                filepath.Join(imageRoot, ".vm-service", "vms"),
		allowedSocketPurposes: allowed,
		guestControlMaxBytes:  guestControlMaxBytes,
		guestImagePolicy:      config.GuestImagePolicy.clone(),
	}, nil
}
