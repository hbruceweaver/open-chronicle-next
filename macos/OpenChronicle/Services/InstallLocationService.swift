import Foundation

enum InstallLocationBlock: String, Equatable, Sendable {
    case mountedVolume = "mounted-volume"
    case appTranslocation = "app-translocation"
    case notApplicationBundle = "not-application-bundle"
    case helperMissing = "helper-missing"
    case helperOutsideBundle = "helper-outside-bundle"
    case helperNotExecutable = "helper-not-executable"
    case managedRootInvalid = "managed-root-invalid"

    var explanation: String {
        switch self {
        case .mountedVolume:
            "Move Open Chronicle to Applications before connecting an AI client."
        case .appTranslocation:
            "macOS is running a quarantined temporary copy. Move Open Chronicle to Applications and reopen it."
        case .notApplicationBundle:
            "Agent setup is available only from the packaged Open Chronicle application."
        case .helperMissing:
            "This Open Chronicle build does not contain the Chronicle MCP helper."
        case .helperOutsideBundle:
            "The Chronicle MCP helper is not contained by this application bundle."
        case .helperNotExecutable:
            "The bundled Chronicle MCP helper is not executable. Reinstall Open Chronicle."
        case .managedRootInvalid:
            "The local Chronicle data directory is not ready."
        }
    }
}

enum InstallLocationAssessment: Equatable, Sendable {
    case ready
    case blocked(InstallLocationBlock)
}

struct InstallLocationService {
    private let fileManager: FileManager

    init(fileManager: FileManager = .default) {
        self.fileManager = fileManager
    }

    func assess(
        applicationBundleURL: URL,
        helperURL: URL,
        managedRootURL: URL
    ) -> InstallLocationAssessment {
        let bundle = applicationBundleURL.standardizedFileURL.resolvingSymlinksInPath()
        let helper = helperURL.standardizedFileURL.resolvingSymlinksInPath()
        let managedRoot = managedRootURL.standardizedFileURL.resolvingSymlinksInPath()

        if bundle.path == "/Volumes" || bundle.path.hasPrefix("/Volumes/") {
            return .blocked(.mountedVolume)
        }
        if bundle.path.contains("/AppTranslocation/") {
            return .blocked(.appTranslocation)
        }
        guard bundle.pathExtension.lowercased() == "app" else {
            return .blocked(.notApplicationBundle)
        }
        guard fileManager.fileExists(atPath: helper.path) else {
            return .blocked(.helperMissing)
        }
        let contentsPrefix = bundle
            .appendingPathComponent("Contents", isDirectory: true)
            .path + "/"
        guard helper.path.hasPrefix(contentsPrefix) else {
            return .blocked(.helperOutsideBundle)
        }
        guard fileManager.isExecutableFile(atPath: helper.path) else {
            return .blocked(.helperNotExecutable)
        }
        var isDirectory: ObjCBool = false
        guard managedRoot.isFileURL,
              managedRoot.path.hasPrefix("/"),
              fileManager.fileExists(atPath: managedRoot.path, isDirectory: &isDirectory),
              isDirectory.boolValue,
              !managedRoot.path.hasPrefix(bundle.path + "/")
        else {
            return .blocked(.managedRootInvalid)
        }
        return .ready
    }
}
