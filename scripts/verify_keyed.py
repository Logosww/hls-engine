#!/usr/bin/env python3
"""Independently inspect and decode keyed prepared outputs against clear references."""
import argparse
import json
import tempfile
from pathlib import Path
from fractions import Fraction
from verify_media import ROOT, run, packets, normalized_payload, decode_and_seek


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ffmpeg', action='store_true')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix='hls-keyed-output-') as tmp:
        command = ['cargo', 'run', '--offline', '--quiet', '--example', 'keyed_export']
        if args.ffmpeg:
            command += ['--features', 'ffmpeg-finalize']
        run(*command, '--', tmp)
        folder = Path(tmp)
        cases = json.loads((folder/'manifest.json').read_text())
        for case in cases:
            expected, actual = packets(folder/case['reference']), packets(folder/case['output'])
            assert actual.keys() == expected.keys(), case
            for kind, rows in expected.items():
                assert len(rows) == len(actual[kind]), case
                for (p, s), (q, t) in zip(rows, actual[kind]):
                    assert s['codec_name'] == t['codec_name'], case
                    for field in ('dts', 'pts', 'duration'):
                        assert abs(int(p[field])*Fraction(s['time_base']) - int(q[field])*Fraction(t['time_base'])) <= Fraction(t['time_base']), (case, kind, field)
                    assert normalized_payload(p, s, 'fmp4') == normalized_payload(q, t, 'fmp4'), (case, kind)
            decode_and_seek(folder/case['output'])
        print(json.dumps({'outputs':len(cases), 'packet_payload_timing_equal':True, 'decoded_and_seeked':True, 'ffmpeg_backend':args.ffmpeg}))

if __name__ == '__main__':
    main()
