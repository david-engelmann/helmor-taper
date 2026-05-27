// record-window.swift
//
// Window-buffer screen recorder. Captures the pixels owned by a specific
// CGWindowID using ScreenCaptureKit (macOS 12.3+). Unlike
// `screencapture -v -l<id>`, which records the screen region at the
// window's coordinates and so picks up anything that overlaps, this
// records the window's own backing buffer — overlapping apps never
// appear in the output.
//
// Usage:
//   swift record-window.swift <owner-substring> <duration-seconds> <out.mov>
//
// `<owner-substring>` is matched case-insensitively against
// kCGWindowOwnerName (e.g. "Warp", "stable", "warp-oss"). The first
// matching on-screen window at layer 0 (normal app window) is recorded.

import AVFoundation
import Cocoa
import CoreMedia
import ScreenCaptureKit

// Bring up NSApplication so Core Graphics is initialized; without this,
// SCContentFilter init trips CGS_REQUIRE_INIT and crashes.
private let _app: Void = {
    let app = NSApplication.shared
    app.setActivationPolicy(.accessory)
    _ = app
}()

@available(macOS 12.3, *)
final class WindowRecorder: NSObject, SCStreamDelegate, SCStreamOutput {
    let outputURL: URL
    let writer: AVAssetWriter
    let videoInput: AVAssetWriterInput
    var firstFrameTime: CMTime?
    let done = DispatchSemaphore(value: 0)

    init(outputURL: URL, width: Int, height: Int) throws {
        self.outputURL = outputURL
        try? FileManager.default.removeItem(at: outputURL)
        self.writer = try AVAssetWriter(outputURL: outputURL, fileType: .mov)
        let settings: [String: Any] = [
            AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: width,
            AVVideoHeightKey: height,
            AVVideoCompressionPropertiesKey: [
                AVVideoAverageBitRateKey: 4_000_000,
                AVVideoMaxKeyFrameIntervalKey: 60,
            ],
        ]
        self.videoInput = AVAssetWriterInput(mediaType: .video, outputSettings: settings)
        self.videoInput.expectsMediaDataInRealTime = true
        if writer.canAdd(videoInput) {
            writer.add(videoInput)
        }
        super.init()
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sampleBuffer.isValid else { return }
        guard let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let info = attachments.first,
              let statusRaw = info[.status] as? Int,
              let status = SCFrameStatus(rawValue: statusRaw),
              status == .complete
        else { return }

        let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        if firstFrameTime == nil {
            firstFrameTime = pts
            writer.startWriting()
            writer.startSession(atSourceTime: pts)
        }
        if videoInput.isReadyForMoreMediaData {
            videoInput.append(sampleBuffer)
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        FileHandle.standardError.write("stream stopped with error: \(error)\n".data(using: .utf8)!)
        done.signal()
    }

    func finish() {
        videoInput.markAsFinished()
        writer.finishWriting { [weak self] in
            self?.done.signal()
        }
    }

    func waitDone() {
        done.wait()
    }
}

@available(macOS 12.3, *)
func main() async {
    let args = CommandLine.arguments
    guard args.count == 4 else {
        FileHandle.standardError.write("usage: record-window.swift <owner-substring> <duration-seconds> <out.mov>\n".data(using: .utf8)!)
        exit(2)
    }
    let needle = args[1].lowercased()
    let duration = Double(args[2]) ?? 8.0
    let outPath = (args[3] as NSString).expandingTildeInPath
    let outURL = URL(fileURLWithPath: outPath)

    let content: SCShareableContent
    do {
        content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
    } catch {
        FileHandle.standardError.write("could not query shareable content: \(error)\n".data(using: .utf8)!)
        exit(1)
    }

    let candidate = content.windows.first { w in
        let owner = (w.owningApplication?.applicationName ?? "").lowercased()
        return owner.contains(needle) && w.windowLayer == 0
    }
    guard let window = candidate else {
        FileHandle.standardError.write("no on-screen window with owner matching \(args[1])\n".data(using: .utf8)!)
        exit(1)
    }
    FileHandle.standardError.write("found window: id=\(window.windowID) owner=\(window.owningApplication?.applicationName ?? "?") title=\(window.title ?? "?") frame=\(window.frame)\n".data(using: .utf8)!)

    let width = Int(window.frame.width)
    let height = Int(window.frame.height)
    let recorder: WindowRecorder
    do {
        recorder = try WindowRecorder(outputURL: outURL, width: width, height: height)
    } catch {
        FileHandle.standardError.write("could not create writer: \(error)\n".data(using: .utf8)!)
        exit(1)
    }

    let filter = SCContentFilter(desktopIndependentWindow: window)
    let config = SCStreamConfiguration()
    config.width = width
    config.height = height
    config.minimumFrameInterval = CMTime(value: 1, timescale: 30)
    config.queueDepth = 6
    config.showsCursor = true

    let stream = SCStream(filter: filter, configuration: config, delegate: recorder)
    do {
        try stream.addStreamOutput(recorder, type: .screen, sampleHandlerQueue: .global(qos: .userInitiated))
        try await stream.startCapture()
    } catch {
        FileHandle.standardError.write("startCapture failed: \(error)\n".data(using: .utf8)!)
        exit(1)
    }

    try? await Task.sleep(nanoseconds: UInt64(duration * 1_000_000_000))

    do {
        try await stream.stopCapture()
    } catch {
        FileHandle.standardError.write("stopCapture failed: \(error)\n".data(using: .utf8)!)
    }
    recorder.finish()
    recorder.waitDone()

    let attrs = try? FileManager.default.attributesOfItem(atPath: outPath)
    let size = (attrs?[.size] as? NSNumber)?.intValue ?? 0
    FileHandle.standardError.write("wrote \(outPath) (\(size) bytes)\n".data(using: .utf8)!)
}

if #available(macOS 12.3, *) {
    let runtime = Task {
        await main()
        exit(0)
    }
    _ = runtime
    RunLoop.main.run()
} else {
    FileHandle.standardError.write("ScreenCaptureKit requires macOS 12.3 or newer.\n".data(using: .utf8)!)
    exit(1)
}
