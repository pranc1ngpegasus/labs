// swift-tools-version: 5.9
import PackageDescription

let rustLibSearchPath = "../target/debug"

let package = Package(
  name: "koe-native",
  platforms: [
    .macOS(.v14)
  ],
  products: [
    .library(
      name: "koe-native",
      type: .dynamic,
      targets: ["koe-native"]
    )
  ],
  targets: [
    .target(
      name: "KoeFfi",
      path: "generated",
      publicHeadersPath: ".",
      linkerSettings: [
        .linkedLibrary("koe_ffi"),
        .unsafeFlags(["-L", rustLibSearchPath], .when(platforms: [.macOS])),
      ]
    ),
    .target(
      name: "koe-native",
      dependencies: ["KoeFfi"],
      path: "Sources/koe-native",
      linkerSettings: [
        .linkedFramework("AppKit"),
        .linkedFramework("AVFoundation"),
        .linkedFramework("Speech"),
      ]
    ),
    .testTarget(
      name: "koe-nativeTests",
      dependencies: ["koe-native", "KoeFfi"],
      path: "Tests/koe-nativeTests"
    ),
  ]
)
