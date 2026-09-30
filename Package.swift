// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "claude-helper",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(name: "AgentPin", targets: ["AgentPin"])
    ],
    targets: [
        .executableTarget(
            name: "AgentPin",
            path: "Sources/AgentPin"
        )
    ]
)
