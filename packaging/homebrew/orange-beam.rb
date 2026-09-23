# Homebrew cask for Orange Beam (橙现). Casks install .app bundles; a formula
# would be wrong for a menu bar app. Publish this file as
# Casks/orange-beam.rb in a tap named woyin/homebrew-orangebeam, then:
#
#   brew install --cask woyin/orangebeam/orange-beam
#
# After each release, update version and sha256 (shasum -a 256 of the zip).
cask "orange-beam" do
  version "1.0-beta"
  sha256 "b609f8690ea884cb264ed77a5fbbe9eed1d17f68785d7f59e5c79440785d6d2a"

  url "https://github.com/woyin/OrangeBeam/releases/download/v#{version}/Orange-Beam-macos-arm64.zip"
  name "Orange Beam"
  name "橙现"
  desc "Presentation effects for the first-generation Logitech Spotlight"
  homepage "https://github.com/woyin/OrangeBeam"

  # The default GitHub strategy ignores pre-releases, and 1.0-beta is one.
  livecheck do
    url :url
    regex(/^v?(\d+(?:\.\d+)*(?:-[a-z0-9.]+)?)$/i)
    strategy :github_releases do |json, regex|
      json.filter_map do |release|
        next if release["draft"]

        release["tag_name"]&.[](regex, 1)
      end
    end
  end

  # AppKit overlays, Apple Silicon only; the bundle targets macOS 13+.
  depends_on arch: :arm64
  depends_on macos: :ventura

  app "Orange Beam.app"

  zap trash: "~/Library/Application Support/Orange Beam"

  caveats <<~EOS
    Orange Beam is ad-hoc signed and not notarized, so Gatekeeper blocks the
    first launch. Either allow it once in System Settings > Privacy & Security
    ("Open Anyway"), or remove the download quarantine:

      xattr -dr com.apple.quarantine "/Applications/Orange Beam.app"

    The app then asks for Input Monitoring (to read the remote). Accessibility
    (to send a play shortcut on a long press) and Screen Recording (for the
    magnifier) are only needed for those features; the control panel shows
    all three.
  EOS
end
