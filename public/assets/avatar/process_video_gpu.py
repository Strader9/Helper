"""
PC Guardian - 视频人像GPU抠像处理脚本
使用 rembg + onnxruntime-gpu 对视频帧进行AI抠像
输出透明背景动态 WebP 动画
"""
import os
import sys
import time
import cv2
import numpy as np
from PIL import Image
from rembg import remove, new_session

# ============================================================
# 配置
# ============================================================
VIDEO_PATH = r"C:\Users\34359\Videos\屏幕录制\929fd03c6533edcff7b8fb713cc24878.mp4"
OUTPUT_DIR = r"C:\大项目\helper电脑\pc-guardian\public\assets\avatar"
OUTPUT_WEBP = os.path.join(OUTPUT_DIR, "avatar_animated.webp")
FRAME_DIR = os.path.join(OUTPUT_DIR, "frames")

# 处理参数
TARGET_WIDTH = 360          # 降分辨率宽度（原576→360，高度按比例800）
FRAME_SKIP = 6              # 每N帧取1帧（30fps → 5fps采样）
AVATAR_WIDTH = 220          # 最终人像宽度
AVATAR_HEIGHT = 320         # 最终人像高度
WEBP_FPS = 8                # WebP播放帧率
QUALITY = 85                # WebP质量

# rembg模型：u2net 质量好，isnet-general-use 更快
# GPU模式下用 u2net 效果最佳
MODEL_NAME = "u2net"


def setup_gpu_session():
    """初始化rembg GPU会话"""
    print(f"[INFO] 初始化 rembg GPU 会话，模型: {MODEL_NAME}")
    try:
        session = new_session(MODEL_NAME, providers=["CUDAExecutionProvider"])
        print("[OK] GPU会话创建成功 (CUDAExecutionProvider)")
        return session
    except Exception as e:
        print(f"[WARN] GPU会话失败: {e}")
        print("[INFO] 尝试 CPU 回退...")
        session = new_session(MODEL_NAME, providers=["CPUExecutionProvider"])
        print("[OK] CPU会话创建成功")
        return session


def extract_frames(video_path, target_width, frame_skip):
    """从视频中提取帧并降分辨率"""
    print(f"[INFO] 打开视频: {video_path}")
    cap = cv2.VideoCapture(video_path)
    if not cap.isOpened():
        raise RuntimeError(f"无法打开视频: {video_path}")

    fps = cap.get(cv2.CAP_PROP_FPS)
    total = int(cap.get(cv2.CAP_PROP_FRAME_COUNT))
    orig_w = int(cap.get(cv2.CAP_PROP_FRAME_WIDTH))
    orig_h = int(cap.get(cv2.CAP_PROP_FRAME_HEIGHT))
    print(f"[INFO] 视频: {orig_w}x{orig_h}, {fps}fps, {total}帧")

    target_h = int(target_width * orig_h / orig_w)
    print(f"[INFO] 降分辨率至: {target_width}x{target_h}")
    print(f"[INFO] 每{frame_skip}帧取1帧，预计提取 ~{total // frame_skip} 帧")

    frames = []
    frame_idx = 0
    saved = 0

    while True:
        ret, frame = cap.read()
        if not ret:
            break
        if frame_idx % frame_skip == 0:
            # 降分辨率
            frame_small = cv2.resize(frame, (target_width, target_h), interpolation=cv2.INTER_AREA)
            # BGR → RGB
            frame_rgb = cv2.cvtColor(frame_small, cv2.COLOR_BGR2RGB)
            frames.append(frame_rgb)
            saved += 1
        frame_idx += 1

    cap.release()
    print(f"[OK] 提取完成: {saved} 帧")
    return frames


def remove_background_gpu(frames, session):
    """使用GPU加速的rembg批量抠像"""
    print(f"[INFO] 开始GPU抠像处理 ({len(frames)} 帧)...")
    results = []
    t0 = time.time()

    for i, frame in enumerate(frames):
        # rembg remove 返回 PIL Image (RGBA)
        img_pil = Image.fromarray(frame)
        result = remove(img_pil, session=session)
        results.append(result)
        if (i + 1) % 5 == 0 or i == len(frames) - 1:
            elapsed = time.time() - t0
            avg = elapsed / (i + 1)
            eta = avg * (len(frames) - i - 1)
            print(f"  进度: {i+1}/{len(frames)} | 平均: {avg:.2f}s/帧 | 预计剩余: {eta:.1f}s")

    total_time = time.time() - t0
    print(f"[OK] 抠像完成: {total_time:.1f}s, 平均 {total_time/len(frames):.2f}s/帧")
    return results


def crop_to_person(frames_rgba):
    """裁剪到人像区域（带padding），统一尺寸"""
    print("[INFO] 计算人像边界框...")

    # 合并所有帧的alpha通道找最大边界
    min_x, min_y = float('inf'), float('inf')
    max_x, max_y = 0, 0

    for img in frames_rgba:
        arr = np.array(img)
        alpha = arr[:, :, 3]
        ys, xs = np.where(alpha > 10)
        if len(xs) > 0:
            min_x = min(min_x, xs.min())
            max_x = max(max_x, xs.max())
            min_y = min(min_y, ys.min())
            max_y = max(max_y, ys.max())

    if max_x == 0:
        print("[WARN] 未检测到人像，使用全图")
        return frames_rgba

    # 加padding
    h, w = frames_rgba[0].size[1], frames_rgba[0].size[0]
    pad_x = int((max_x - min_x) * 0.08)
    pad_y = int((max_y - min_y) * 0.05)
    min_x = max(0, min_x - pad_x)
    min_y = max(0, min_y - pad_y)
    max_x = min(w, max_x + pad_x)
    max_y = min(h, max_y + pad_y)

    crop_w = max_x - min_x
    crop_h = max_y - min_y
    print(f"[INFO] 人像区域: ({min_x},{min_y}) - ({max_x},{max_y}), 尺寸: {crop_w}x{crop_h}")

    cropped = []
    for img in frames_rgba:
        c = img.crop((min_x, min_y, max_x, max_y))
        cropped.append(c)

    return cropped


def resize_frames(frames, target_w, target_h):
    """统一缩放到目标尺寸，保持比例居中"""
    resized = []
    for img in frames:
        # 创建透明画布
        canvas = Image.new("RGBA", (target_w, target_h), (0, 0, 0, 0))
        # 等比缩放
        img.thumbnail((target_w, target_h), Image.LANCZOS)
        # 居中粘贴
        x = (target_w - img.width) // 2
        y = (target_h - img.height) // 2
        canvas.paste(img, (x, y), img)
        resized.append(canvas)
    return resized


def save_webp(frames, output_path, fps, quality):
    """保存为动态WebP"""
    print(f"[INFO] 保存动态 WebP: {output_path}")
    duration = int(1000 / fps)  # 每帧持续时间(ms)

    frames[0].save(
        output_path,
        save_all=True,
        append_images=frames[1:],
        duration=duration,
        loop=0,
        quality=quality,
        method=6,  # 最高压缩
    )
    size_mb = os.path.getsize(output_path) / 1024 / 1024
    print(f"[OK] WebP已保存: {output_path} ({size_mb:.2f} MB)")


def save_preview_png(frames, output_dir):
    """保存第一帧作为预览"""
    preview_path = os.path.join(output_dir, "avatar_preview.png")
    frames[0].save(preview_path)
    print(f"[OK] 预览图已保存: {preview_path}")


def main():
    os.makedirs(OUTPUT_DIR, exist_ok=True)
    os.makedirs(FRAME_DIR, exist_ok=True)

    print("=" * 60)
    print("PC Guardian - GPU视频人像抠像处理")
    print("=" * 60)

    # 1. 初始化GPU会话
    session = setup_gpu_session()

    # 2. 提取视频帧
    frames = extract_frames(VIDEO_PATH, TARGET_WIDTH, FRAME_SKIP)
    if not frames:
        print("[ERROR] 未提取到帧")
        sys.exit(1)

    # 3. GPU抠像
    frames_rgba = remove_background_gpu(frames, session)

    # 4. 裁剪到人像
    frames_cropped = crop_to_person(frames_rgba)

    # 5. 统一尺寸
    frames_final = resize_frames(frames_cropped, AVATAR_WIDTH, AVATAR_HEIGHT)

    # 6. 保存动态WebP
    save_webp(frames_final, OUTPUT_WEBP, WEBP_FPS, QUALITY)

    # 7. 保存预览
    save_preview_png(frames_final, OUTPUT_DIR)

    print("\n" + "=" * 60)
    print("[完成] 动态人像处理完毕!")
    print(f"  输出: {OUTPUT_WEBP}")
    print(f"  帧数: {len(frames_final)}")
    print(f"  尺寸: {AVATAR_WIDTH}x{AVATAR_HEIGHT}")
    print(f"  帧率: {WEBP_FPS}fps")
    print("=" * 60)


if __name__ == "__main__":
    main()
