// mp4-to-gif.swift
//
// Convert an MP4 (or any AVAsset-readable video) into an animated GIF.
// Uses AVAssetImageGenerator to extract frames at a fixed rate and
// CGImageDestination with `public.gif` UTI to assemble the loop.
//
// GIFs are GitHub's bulletproof inline-video format: they embed in any
// markdown image syntax (`![alt](path)`), no `<video>` tag required.
//
// Usage:
//   swift mp4-to-gif.swift <input.mp4> <output.gif> [fps] [maxWidth]
//
//     fps        target frame rate (default 5)
//     maxWidth   downscale so width ≤ this many points (default 720;
//                aspect ratio preserved). Smaller = smaller file.

import AVFoundation
import CoreServices
import Foundation
import ImageIO

let args = CommandLine.arguments
guard args.count >= 3 else {
    FileHandle.standardError.write("usage: mp4-to-gif.swift <input.mp4> <output.gif> [fps=5] [maxWidth=720]\n".data(using: .utf8)!)
    exit(2)
}
let inputURL = URL(fileURLWithPath: args[1])
let outputURL = URL(fileURLWithPath: args[2])
let fps = Double(args.count > 3 ? args[3] : "5") ?? 5
let maxWidth = Int(args.count > 4 ? args[4] : "720") ?? 720

let asset = AVAsset(url: inputURL)
let durationSec = CMTimeGetSeconds(asset.duration)
guard durationSec.isFinite, durationSec > 0 else {
    FileHandle.standardError.write("input has no duration / unreadable\n".data(using: .utf8)!)
    exit(1)
}
let frameDelay = 1.0 / fps
let frameCount = max(2, Int(durationSec * fps))

guard let track = asset.tracks(withMediaType: .video).first else {
    FileHandle.standardError.write("no video track\n".data(using: .utf8)!)
    exit(1)
}
let nativeSize = track.naturalSize.applying(track.preferredTransform)
let nativeW = abs(nativeSize.width)
let nativeH = abs(nativeSize.height)
let scale = nativeW > CGFloat(maxWidth) ? CGFloat(maxWidth) / nativeW : 1.0
let outW = Int(nativeW * scale)
let outH = Int(nativeH * scale)

let generator = AVAssetImageGenerator(asset: asset)
generator.appliesPreferredTrackTransform = true
generator.maximumSize = CGSize(width: outW, height: outH)
generator.requestedTimeToleranceBefore = .zero
generator.requestedTimeToleranceAfter = .zero

guard let destination = CGImageDestinationCreateWithURL(
    outputURL as CFURL,
    kUTTypeGIF,
    frameCount,
    nil
) else {
    FileHandle.standardError.write("could not create GIF destination at \(outputURL.path)\n".data(using: .utf8)!)
    exit(1)
}

let fileProps: [CFString: Any] = [
    kCGImagePropertyGIFDictionary: [
        kCGImagePropertyGIFLoopCount: 0  // 0 = loop forever
    ] as [CFString: Any]
]
CGImageDestinationSetProperties(destination, fileProps as CFDictionary)

let frameProps: [CFString: Any] = [
    kCGImagePropertyGIFDictionary: [
        kCGImagePropertyGIFUnclampedDelayTime: frameDelay,
        kCGImagePropertyGIFDelayTime: frameDelay,
    ] as [CFString: Any]
]

var addedFrames = 0
for i in 0..<frameCount {
    let t = CMTime(seconds: Double(i) / fps, preferredTimescale: 600)
    do {
        let cgImage = try generator.copyCGImage(at: t, actualTime: nil)
        CGImageDestinationAddImage(destination, cgImage, frameProps as CFDictionary)
        addedFrames += 1
    } catch {
        FileHandle.standardError.write("skip frame at \(t.seconds)s: \(error)\n".data(using: .utf8)!)
    }
}

guard CGImageDestinationFinalize(destination) else {
    FileHandle.standardError.write("CGImageDestinationFinalize failed\n".data(using: .utf8)!)
    exit(1)
}

let size = (try? FileManager.default.attributesOfItem(atPath: outputURL.path))?[.size] as? NSNumber ?? 0
FileHandle.standardError.write("wrote \(outputURL.path) (\(size.intValue) bytes, \(addedFrames) frames @ \(fps)fps, \(outW)x\(outH))\n".data(using: .utf8)!)
