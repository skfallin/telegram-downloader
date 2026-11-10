from download import file_name

assert file_name(5, "foto.jpg", ".jpg") == "5_foto.jpg"
assert file_name(5, None, ".mp4") == "5_file.mp4"
assert file_name(5, "../../etc/passwd", None) == "5_passwd"
assert file_name(5, "cartella/", ".pdf") == "5_file.pdf"
print("ok")
