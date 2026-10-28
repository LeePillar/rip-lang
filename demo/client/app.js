const TOKEN_KEY = 'rip_admin_token';

// 与 Rip 后端共享的会话状态：token 保存在 localStorage
const session = {
    token: localStorage.getItem(TOKEN_KEY) || ''
};

async function api(method, path, body) {
    const headers = {};
    if (body !== undefined) {
        headers['Content-Type'] = 'application/json';
    }
    if (session.token) {
        headers['Authorization'] = 'Bearer ' + session.token;
    }

    let response;
    try {
        response = await fetch(path, {
            method,
            headers,
            body: body === undefined ? undefined : JSON.stringify(body)
        });
    } catch (error) {
        return { status: 0, payload: { message: '网络错误：' + error.message } };
    }

    const text = await response.text();
    let payload = {};
    try {
        payload = text ? JSON.parse(text) : {};
    } catch (e) {
        payload = { message: text };
    }
    return { status: response.status, payload };
}

function saveToken(token) {
    session.token = token;
    if (token) {
        localStorage.setItem(TOKEN_KEY, token);
    } else {
        localStorage.removeItem(TOKEN_KEY);
    }
}

const app = Vue.createApp({
    data() {
        return {
            view: 'login',
            busy: false,
            loading: false,
            loginForm: { account: '', password: '' },
            loginError: '',
            me: {},
            users: [],
            total: 0,
            page: 1,
            limit: 10,
            keyword: '',
            statusFilter: '',
            searchTimer: null,
            form: {
                open: false,
                mode: 'create',
                id: 0,
                name: '',
                email: '',
                phone: '',
                password: '',
                role: 'user',
                status: 'active',
                error: ''
            },
            toast: '',
            toastTimer: null
        };
    },
    computed: {
        totalPages() {
            return Math.max(1, Math.ceil(this.total / this.limit));
        }
    },
    mounted() {
        this.bootstrap();
    },
    methods: {
        async bootstrap() {
            if (!session.token) {
                return;
            }
            const res = await api('GET', '/api/me');
            if (res.status === 200) {
                this.me = res.payload.data || {};
                this.view = 'app';
                await this.loadUsers();
            } else {
                saveToken('');
            }
        },

        async submitLogin() {
            if (!this.loginForm.account || !this.loginForm.password) {
                this.loginError = '请输入账号和密码';
                return;
            }
            this.busy = true;
            this.loginError = '';
            const res = await api('POST', '/api/login', {
                account: this.loginForm.account,
                password: this.loginForm.password
            });
            this.busy = false;

            if (res.status !== 200) {
                this.loginError = res.payload.message || '登录失败';
                return;
            }
            saveToken(res.payload.data.token);
            this.me = res.payload.data.user || {};
            this.loginForm.password = '';
            this.view = 'app';
            this.page = 1;
            await this.loadUsers();
        },

        async logout() {
            await api('POST', '/api/logout');
            saveToken('');
            this.view = 'login';
            this.me = {};
            this.users = [];
            this.total = 0;
            this.keyword = '';
            this.statusFilter = '';
            this.loginForm.account = '';
            this.loginForm.password = '';
            this.loginError = '';
        },

        async loadUsers() {
            this.loading = true;
            const params = new URLSearchParams();
            params.set('page', String(this.page));
            params.set('limit', String(this.limit));
            if (this.keyword) {
                params.set('keyword', this.keyword);
            }
            if (this.statusFilter) {
                params.set('status', this.statusFilter);
            }

            const res = await api('GET', '/api/users?' + params.toString());
            this.loading = false;

            if (res.status === 401) {
                return this.unauthorized();
            }
            if (res.status !== 200) {
                return this.showToast(res.payload.message || '加载失败');
            }
            const data = res.payload.data || {};
            this.users = data.items || [];
            this.total = data.total || 0;
            this.page = data.page || 1;
        },

        onSearch() {
            clearTimeout(this.searchTimer);
            this.searchTimer = setTimeout(() => {
                this.page = 1;
                this.loadUsers();
            }, 300);
        },

        reload() {
            this.page = 1;
            this.loadUsers();
        },

        goPage(target) {
            if (target < 1 || target > this.totalPages || target === this.page) {
                return;
            }
            this.page = target;
            this.loadUsers();
        },

        openCreate() {
            this.form = {
                open: true,
                mode: 'create',
                id: 0,
                name: '',
                email: '',
                phone: '',
                password: '',
                role: 'user',
                status: 'active',
                error: ''
            };
        },

        openEdit(user) {
            this.form = {
                open: true,
                mode: 'edit',
                id: user.id,
                name: user.name,
                email: user.email,
                phone: user.phone || '',
                password: '',
                role: user.role || 'user',
                status: user.status || 'active',
                error: ''
            };
        },

        closeForm() {
            this.form.open = false;
            this.form.error = '';
        },

        async saveUser() {
            const form = this.form;
            if (!form.name || form.name.length < 2) {
                form.error = '姓名至少 2 个字符';
                return;
            }
            if (!form.email || form.email.indexOf('@') < 0) {
                form.error = '邮箱格式不正确';
                return;
            }
            if (form.mode === 'create' && (!form.password || form.password.length < 6)) {
                form.error = '密码至少 6 个字符';
                return;
            }
            if (form.password && form.password.length < 6) {
                form.error = '密码至少 6 个字符';
                return;
            }

            this.busy = true;
            form.error = '';

            const body = {
                name: form.name,
                email: form.email,
                phone: form.phone,
                role: form.role,
                status: form.status
            };
            if (form.password) {
                body.password = form.password;
            }

            const res = form.mode === 'create'
                ? await api('POST', '/api/users', body)
                : await api('PUT', '/api/users/' + form.id, body);
            this.busy = false;

            if (res.status === 401) {
                return this.unauthorized();
            }
            if (res.status === 200 || res.status === 201) {
                const created = form.mode === 'create';
                this.form.open = false;
                this.showToast(created ? '已新增用户' : '已保存修改');
                await this.loadUsers();
                return;
            }
            form.error = res.payload.message || '保存失败';
        },

        async removeUser(user) {
            if (!window.confirm('确认删除用户「' + user.name + '」？')) {
                return;
            }
            const res = await api('DELETE', '/api/users/' + user.id);
            if (res.status === 401) {
                return this.unauthorized();
            }
            if (res.status === 200) {
                this.showToast('已删除');
                if (this.users.length === 1 && this.page > 1) {
                    this.page -= 1;
                }
                await this.loadUsers();
                return;
            }
            this.showToast(res.payload.message || '删除失败');
        },

        unauthorized() {
            saveToken('');
            this.view = 'login';
            this.me = {};
            this.users = [];
            this.total = 0;
            this.loginError = '登录已过期，请重新登录';
        },

        showToast(message) {
            this.toast = message;
            clearTimeout(this.toastTimer);
            this.toastTimer = setTimeout(() => {
                this.toast = '';
            }, 2200);
        },

        roleLabel(role) {
            return role === 'admin' ? '管理员' : '普通用户';
        },

        statusLabel(status) {
            return status === 'disabled' ? '已禁用' : '正常';
        }
    }
});

app.mount('#app');
