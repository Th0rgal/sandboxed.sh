// swift-tools-version:6.0
import PackageDescription

let package = Package(
    name: "SandboxedDashboard",
    platforms: [
        .iOS(.v18)
    ],
    products: [
        .library(name: "SandboxedDashboard", targets: ["SandboxedDashboard"])
    ],
    targets: [
        .target(
            name: "SandboxedDashboard",
            path: "SandboxedDashboard"
        )
    ]
)
