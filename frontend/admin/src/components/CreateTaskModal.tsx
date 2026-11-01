import { useEffect, useState } from 'react';
import { App as AntApp, AutoComplete, Button, Form, Typography } from 'antd';
import { PlusOutlined } from '@ant-design/icons';
import { ModalForm, ProFormSelect, ProFormSwitch } from '@ant-design/pro-components';
import { api, parseUserInput } from '../api';
import { CATEGORY_OPTIONS, STATUS_OPTIONS } from '../labels';
import { useNoSpellCheck } from '../no-spellcheck';

/** 新建同步任务：豆瓣用户在表单内选择（已知用户自动补全，也可输入新 uid 完成冷启动） */
export default function CreateTaskModal({
  onCreated,
  /** 预置用户（用户页行内发起）：隐藏用户输入、标题带出该用户 */
  presetUser,
  /** 触发按钮形态：任务页工具栏 primary / 用户页行内 link */
  buttonType = 'primary',
}: {
  onCreated: (taskId: string) => void;
  presetUser?: string;
  buttonType?: 'primary' | 'link';
}) {
  const { message } = AntApp.useApp();
  const [userOptions, setUserOptions] = useState<{ value: string; label: string }[]>([]);
  // 用户输入框收 ID/链接，macOS 上关掉自动拼写订正
  const userRef = useNoSpellCheck();

  // 已知用户做自动补全（旧版后端无 /users 端点时回退空选项，自由输入不受影响）
  useEffect(() => {
    api
      .listUsers()
      .then((res) =>
        setUserOptions(
          res.users.map((u) => ({
            value: u.uid,
            label: u.username ? `${u.username}（@${u.uid}）` : `@${u.uid}`,
          })),
        ),
      )
      .catch(() => setUserOptions([]));
  }, []);

  return (
    <ModalForm
      title={presetUser ? `新建同步任务 · @${presetUser}` : '新建同步任务'}
      width={520}
      trigger={
        <Button type={buttonType} icon={buttonType === 'primary' ? <PlusOutlined /> : undefined} size={buttonType === 'link' ? 'small' : 'middle'}>
          {presetUser ? '同步' : '新建同步'}
        </Button>
      }
      modalProps={{ destroyOnClose: true }}
      initialValues={{
        user: presetUser,
        categories: ['movie', 'book', 'music'],
        statuses: ['collect'],
        refresh: false,
      }}
      onFinish={async (values) => {
        const user = presetUser ?? parseUserInput(values.user || '');
        if (!user) {
          message.error('用户 ID 不合法：仅限字母、数字、.、-、_，或粘贴豆瓣个人主页链接');
          return false;
        }
        try {
          const res = await api.createTask({
            user,
            categories: values.categories,
            statuses: values.statuses,
            refresh: values.refresh ?? false,
          });
          message.success(
            `任务已创建（缓存命中 ${res.cached_dimensions ?? 0} 个维度，新建 ${res.created_dimensions ?? 0} 个维度）`,
          );
          onCreated(res.task_id);
          return true;
        } catch (e) {
          message.error((e as Error).message);
          return false;
        }
      }}
    >
      <Typography.Text type="secondary" style={{ display: 'block', marginBottom: 16 }}>
        同步约需 1–3 分钟（受豆瓣限速约束），完成后数据入库并可在数据浏览页查看
      </Typography.Text>
      {!presetUser && (
        <Form.Item
          name="user"
          label="豆瓣用户"
          rules={[{ required: true, message: '选择已知用户或输入新用户 ID / 主页链接' }]}
          extra="输入框支持已知用户昵称/ID 检索，也可直接粘贴新用户主页链接为其创建首个同步"
        >
          <AutoComplete
            ref={userRef}
            options={userOptions}
            filterOption={(input, option) =>
              (option?.value ?? '').toLowerCase().includes(input.toLowerCase()) ||
              (option?.label ?? '').toLowerCase().includes(input.toLowerCase())
            }
            placeholder="如 wenjun30、芝麻，或 https://www.douban.com/people/wenjun30/"
          />
        </Form.Item>
      )}
      <ProFormSelect
        name="categories"
        label="类别"
        mode="multiple"
        options={CATEGORY_OPTIONS}
        rules={[{ required: true, message: '至少选择一个类别' }]}
      />
      <ProFormSelect
        name="statuses"
        label="标记状态"
        mode="multiple"
        options={STATUS_OPTIONS}
        rules={[{ required: true, message: '至少选择一个状态' }]}
      />
      <ProFormSwitch name="refresh" label="强制刷新缓存" extra="忽略未过期缓存，重新回源抓取" />
    </ModalForm>
  );
}
