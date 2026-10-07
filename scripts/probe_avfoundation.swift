// macOS acceptance probe. Track discovery/selection does not certify subtitle rendering.
import Foundation
import AVFoundation

final class LegibleProbe: NSObject, AVPlayerItemLegibleOutputPushDelegate {
    var cues: [[String: Any]] = []
    func legibleOutput(_ output: AVPlayerItemLegibleOutput, didOutputAttributedStrings strings: [NSAttributedString],
                       nativeSampleBuffers: [Any], forItemTime time: CMTime) {
        for string in strings {
            var attributes: [[String: String]] = []
            string.enumerateAttributes(in: NSRange(location: 0, length: string.length)) { value, _, _ in
                attributes.append(Dictionary(uniqueKeysWithValues: value.map { key, value in
                    (key.rawValue, String(describing: value))
                }))
            }
            cues.append(["text": string.string, "time": time.seconds, "attributes": attributes])
        }
    }
}

@main struct Probe {
    @MainActor static func main() async {
        var rows: [[String: Any]] = []
        for file in CommandLine.arguments.dropFirst() {
            let asset = AVURLAsset(url: URL(fileURLWithPath: file))
            do {
                let tracks = try await asset.load(.tracks)
                let item = AVPlayerItem(asset: asset)
                var row: [String: Any] = ["file": file, "tracks": tracks.map { ["id": $0.trackID, "type": $0.mediaType.rawValue] }]
                for (name, characteristic) in [("audio", AVMediaCharacteristic.audible), ("subtitles", .legible)] {
                    if let group = try await asset.loadMediaSelectionGroup(for: characteristic) {
                        row[name] = group.options.map { ["name": $0.displayName, "language": $0.extendedLanguageTag ?? "und"] }
                        var selected = true
                        for option in group.options {
                            item.select(option, in: group)
                            selected = selected && item.currentMediaSelection.selectedMediaOption(in: group) == option
                        }
                        row[name+"Selection"] = selected && !group.options.isEmpty
                    } else { row[name] = []; row[name+"Selection"] = false }
                }
                row["playable"] = try await asset.load(.isPlayable)
                let probe = LegibleProbe()
                let output = AVPlayerItemLegibleOutput()
                output.setDelegate(probe, queue: .main)
                item.add(output)
                let player = AVPlayer(playerItem: item)
                player.isMuted = true
                var decoded: [[String: Any]] = []
                if let group = try await asset.loadMediaSelectionGroup(for: .legible) {
                    for option in group.options {
                        item.select(option, in: group)
                        probe.cues = []
                        await player.seek(to: CMTime(seconds: 0.1, preferredTimescale: 90000), toleranceBefore: .zero, toleranceAfter: .zero)
                        player.play()
                        try await Task.sleep(nanoseconds: 5_300_000_000)
                        player.pause()
                        decoded.append(["language": option.extendedLanguageTag ?? "und", "cues": probe.cues,
                                        "playerTime": player.currentTime().seconds, "status": item.status.rawValue])
                    }
                }
                row["decodedSubtitles"] = decoded
                row["subtitleRendering"] = "AVPlayerItemLegibleOutput decode only; native display verified separately by the companion WebKit probe"
                player.replaceCurrentItem(with: nil)
                rows.append(row)
            } catch {rows.append(["file":file,"error":String(describing:error)])}
        }
        let evidence: [String: Any] = ["platform":ProcessInfo.processInfo.operatingSystemVersionString,"scope":"AVFoundation discovery, selection and timed attributed subtitle decode; companion WebKit snapshots verify native rendering", "results":rows]
        let data = try! JSONSerialization.data(withJSONObject:evidence,options:[.prettyPrinted,.sortedKeys])
        print(String(data:data,encoding:.utf8)!)
    }
}
