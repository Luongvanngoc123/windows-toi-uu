# Velo

Ứng dụng desktop viết bằng Rust cho Windows 10/11 x64. Giao diện native bằng egui/eframe, không cần WebView, Node.js hoặc PowerShell để chạy phần lõi.

## Chạy

Mở `Velo.exe` trong thư mục phát hành. App mặc định yêu cầu quyền **Administrator** qua hộp thoại UAC của Windows, rồi mới mở giao diện. Nếu hủy UAC, app không chạy tiếp. Chế độ chẩn đoán và kiểm tra render chỉ đọc không yêu cầu nâng quyền.

Ứng dụng không tự tối ưu lúc mở. Bạn chọn từng thao tác trong giao diện.

## Có gì

- CPU, RAM, thời gian hoạt động và tiến trình dùng nhiều RAM, cập nhật mỗi 2 giây.
- Bật/tắt mục Run và thư mục Startup bằng cơ chế StartupApproved của Windows, không xóa lệnh gốc.
- Danh sách chặn tên miền tải ứng dụng, có cấu hình sẵn cho WPS Office và 360 Security. Thêm ứng dụng của bạn bằng tên và các tên miền tải chính xác.
- Sao lưu hosts trước khi thay đổi và gỡ riêng quy tắc do Velo quản lý.
- Quét rồi dọn file tạm người dùng cũ hơn 24 giờ; bỏ qua file đang dùng và đường dẫn liên kết.
- Thu gọn working set RAM theo yêu cầu.
- Chuyển giữa chế độ nguồn Cân bằng và Hiệu năng cao.
- Kiểm tra Visual C++ 2015+ x86/x64, .NET Desktop 8/10 x64 và WebView2; tự cài các gói còn thiếu được chọn bằng WinGet.
- Liên kết WinUtil chính thức của Chris Titus Tech, lệnh có thể sao chép, và lối mở cài đặt Windows.
- Nhật ký kết quả trong phiên làm việc.

## Giới hạn cần biết

Chặn hosts chỉ áp dụng cho tên miền đã liệt kê, không hỗ trợ wildcard. Nguồn tải khác, kết nối trực tiếp IP, proxy hoặc bộ phân giải DNS riêng có thể vượt qua. Quy tắc cũng có thể ảnh hưởng website/cập nhật của nhà cung cấp. Đây không phải cơ chế cấm cài đặt hoặc khóa thực thi ứng dụng.

Quản lý startup không bao gồm dịch vụ Windows, scheduled task hoặc mọi ứng dụng đóng gói Microsoft Store. Ứng dụng có thể tự đăng ký lại startup sau khi cập nhật.

Thu gọn RAM chỉ có hiệu quả tạm thời; ứng dụng sẽ nạp lại bộ nhớ khi cần, đôi khi làm thao tác tiếp theo chậm hơn. Không có bộ dọn RAM chạy ngầm tự động. Chế độ nguồn Hiệu năng cao có thể tăng nhiệt và hao pin; một số máy Modern Standby không hỗ trợ chế độ này.

File tạm đã dọn không đi vào Recycle Bin và không thể hoàn tác. Có bước xác nhận trước khi xóa. Hãy đóng bộ cài và công việc quan trọng trước khi dọn.

Velo chỉ liên kết tới WinUtil. Lệnh `irm https://christitus.com/win | iex` tải và thực thi mã bên ngoài, do người dùng chủ động chạy. Velo không phải sản phẩm của Chris Titus Tech.

Mục **Thư viện** cần WinGet (App Installer) và Internet. Chọn các gói, rồi bấm cài một lượt. Có bước xác nhận điều khoản nguồn và giấy phép gói. Nếu chưa có WinGet, app mở trang App Installer chính thức của Microsoft để bạn cài. Phát hiện runtime dựa trên Registry/thư mục cài chuẩn; không bao gồm mọi thư viện của từng game hoặc yêu cầu phiên bản riêng của từng ứng dụng. Không tự cài khi mở Velo.

## Dữ liệu và hoàn tác

Cấu hình và bản sao lưu nằm trong `%LOCALAPPDATA%\Velo`. Mục **Gỡ toàn bộ chặn** chỉ loại bỏ phần hosts có đánh dấu của Velo, giữ phần còn lại. Bật lại startup bằng cùng công tắc trong ứng dụng. Chọn **Cân bằng** để trở về chế độ nguồn tiêu chuẩn.

Không gửi thông tin hệ thống tới máy chủ. Đường dẫn công cụ chính thức chỉ mở khi bạn bấm. Không tắt Defender, Windows Update hoặc UAC.

## Build từ mã nguồn

Cài Rust stable cho Windows x64 và linker tương ứng (Visual Studio Build Tools với C++ cho MSVC, hoặc toolchain GNU Windows). Tại thư mục có `Cargo.toml`:

```powershell
cargo test --locked
cargo build --release --locked
```

File kết quả: `target\release\velo.exe`. Windows 10/11 x64 là nền tảng mục tiêu; không hỗ trợ Linux/macOS trong bản này.

Chẩn đoán chỉ đọc, dùng bản debug để xem stdout trong terminal:

```powershell
cargo run --locked -- --diagnose
```
