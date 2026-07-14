import Foundation

enum AppRoute: Hashable {
    case home
    case timeline
    case chunk(String)
    case event(String)
    case analysis(String)
    case settings
}

@MainActor
final class NavigationModel: ObservableObject {
    @Published var path: [AppRoute] = []
    @Published var selectedRange: ClosedRange<Date>?
    @Published var filterText = ""

    func show(_ route: AppRoute) {
        if route == .home {
            path.removeAll()
        } else {
            path.append(route)
        }
    }

    func goBack() {
        guard !path.isEmpty else { return }
        path.removeLast()
    }
}
