// mov-to-mp4.swift
//
// Container remux from QuickTime (.mov) to MP4 (.mp4) using AVFoundation.
// No re-encode — the underlying H.264 video is copied verbatim. Used to
// produce a browser-friendly demo asset for the README; most browsers
// don't natively play .mov, but every browser plays .mp4 with H.264.
//
// Usage: swift mov-to-mp4.swift <input.mov> <output.mp4>

import AVFoundation
import Foundation

let args = CommandLine.arguments
guard args.count == 3 else {
    FileHandle.standardError.write("usage: mov-to-mp4.swift <input.mov> <output.mp4>\n".data(using: .utf8)!)
    exit(2)
}

let inURL = URL(fileURLWithPath: args[1])
let outURL = URL(fileURLWithPath: args[2])
try? FileManager.default.removeItem(at: outURL)

let asset = AVAsset(url: inURL)
guard let export = AVAssetExportSession(asset: asset, presetName: AVAssetExportPresetPassthrough) else {
    FileHandle.standardError.write("could not create export session (passthrough not available)\n".data(using: .utf8)!)
    exit(1)
}
export.outputURL = outURL
export.outputFileType = .mp4

let done = DispatchSemaphore(value: 0)
export.exportAsynchronously {
    done.signal()
}
done.wait()

switch export.status {
case .completed:
    let size = (try? FileManager.default.attributesOfItem(atPath: outURL.path))?[.size] as? NSNumber
    FileHandle.standardError.write("wrote \(outURL.path) (\(size?.intValue ?? 0) bytes)\n".data(using: .utf8)!)
    exit(0)
case .failed:
    FileHandle.standardError.write("export failed: \(export.error?.localizedDescription ?? "unknown")\n".data(using: .utf8)!)
    exit(1)
default:
    FileHandle.standardError.write("export status: \(export.status.rawValue)\n".data(using: .utf8)!)
    exit(1)
}
