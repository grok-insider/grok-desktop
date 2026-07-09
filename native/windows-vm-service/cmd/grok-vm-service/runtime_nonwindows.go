//go:build !windows

package main

import (
	"fmt"
	"log/slog"
	"os"
)

func finalizePlatformOptions(options *options) error {
	if options.dataRoot == "" {
		options.dataRoot = os.Getenv("GROK_VM_DATA_ROOT")
	}
	if options.developmentUserSID == "" {
		options.developmentUserSID = os.Getenv("GROK_VM_DEVELOPMENT_USER_SID")
	}
	if options.releaseRoot == "" {
		options.releaseRoot = os.Getenv("GROK_VM_RELEASE_ROOT")
	}
	compiledTrust, err := decodeCompiledGuestCatalogTrust()
	if err != nil {
		return err
	}
	options.guestCatalogTrust = compiledTrust
	if options.guestCatalogTrust == "" {
		options.guestCatalogTrust = os.Getenv("GROK_GUEST_CATALOG_TRUST")
	}
	if options.dataRoot == "" || options.developmentUserSID == "" || options.releaseRoot == "" || options.guestCatalogTrust == "" {
		return fmt.Errorf("--data-root, --development-user-sid, release root, and guest catalog trust are required off Windows")
	}
	return nil
}

func isServiceProcess() (bool, error)        { return false, nil }
func explicitForegroundRequired() bool       { return false }
func allowDevelopmentIdentities() bool       { return true }
func runService(options, *slog.Logger) error { return fmt.Errorf("Windows SCM mode is unavailable") }
